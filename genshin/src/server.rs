//! HTTP server for the artifact manager with origin-based CORS security.
//!
//! Two-thread architecture:
//! - HTTP thread: handles all HTTP I/O (spawned)
//! - Execution thread: owns game controller, processes jobs (original thread)
//!
//! Communication: mpsc channel for job submission, Arc<Mutex<JobState>> for status.
//!
//! Security: Origin header checked against allowlist. Only ggartifact.com and
//! localhost origins are permitted. Requests with disallowed origins are rejected
//! with 403. Non-browser clients (no Origin header) are allowed — CORS is a
//! browser-enforced mechanism.
//!
//! 异步 HTTP 服务器。双线程架构：HTTP 线程处理请求，执行线程控制游戏。
//! 安全：通过 Origin 头限制仅允许 ggartifact.com 和 localhost 来源。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use anyhow::{anyhow, Result};
use log::{error, info, warn};
use tiny_http::{Header, Method, Response, Server};

use crate::manager::models::*;
use crate::manager::orchestrator::ArtifactManager;
use crate::scanner::common::game_controller::GenshinGameController;

/// Maximum request body size (5 MB).
const MAX_BODY_SIZE: usize = 5 * 1024 * 1024;

/// Allowed production origins.
const ALLOWED_ORIGINS: &[&str] = &[
    "https://ggartifact.com",
];

/// Check if an origin is allowed.
///
/// Allows:
/// - `https://ggartifact.com` (production)
/// - `http://localhost[:port]` (development)
/// - `http://127.0.0.1[:port]` (development)
fn is_origin_allowed(origin: &str) -> bool {
    if ALLOWED_ORIGINS.contains(&origin) {
        return true;
    }
    // Allow localhost for development (any port)
    if origin == "http://localhost" || origin.starts_with("http://localhost:") {
        return true;
    }
    if origin == "http://127.0.0.1" || origin.starts_with("http://127.0.0.1:") {
        return true;
    }
    false
}

/// Extract the Origin header from a request.
fn get_origin(request: &tiny_http::Request) -> Option<String> {
    for header in request.headers() {
        if header.field.as_str().as_str().eq_ignore_ascii_case("origin") {
            return Some(header.value.as_str().to_string());
        }
    }
    None
}

/// Check if the game window is currently alive (Windows only).
///
/// Called from the HTTP thread — does not need the game controller.
/// Uses Win32 EnumWindows to search for the game window by title.
///
/// 检查游戏窗口是否存在（仅 Windows）。从 HTTP 线程调用。
#[cfg(target_os = "windows")]
fn is_game_window_alive() -> bool {
    let window_names = ["\u{539F}\u{795E}", "Genshin Impact"]; // 原神
    let handles = yas::utils::iterate_window();
    for hwnd in &handles {
        if let Some(title) = yas::utils::get_window_title(*hwnd) {
            let trimmed = title.trim();
            if window_names.iter().any(|n| trimmed == *n) {
                return true;
            }
        }
    }
    false
}

#[cfg(not(target_os = "windows"))]
fn is_game_window_alive() -> bool {
    true
}

/// CORS headers for an allowed origin.
fn cors_headers(origin: &str) -> Vec<Header> {
    vec![
        Header::from_bytes("Access-Control-Allow-Origin", origin).unwrap(),
        Header::from_bytes("Access-Control-Allow-Methods", "GET, POST, OPTIONS").unwrap(),
        Header::from_bytes("Access-Control-Allow-Headers", "Content-Type").unwrap(),
        Header::from_bytes("Content-Type", "application/json; charset=utf-8").unwrap(),
    ]
}

/// Send a JSON response with optional CORS headers.
///
/// `origin`: the validated origin to echo back, or None for non-browser clients.
fn respond_json(request: tiny_http::Request, status: u16, json: &str, origin: Option<&str>) {
    let mut resp = Response::from_string(json).with_status_code(status);
    if let Some(o) = origin {
        for header in cors_headers(o) {
            resp.add_header(header);
        }
    } else {
        resp.add_header(
            Header::from_bytes("Content-Type", "application/json; charset=utf-8").unwrap(),
        );
    }
    if let Err(e) = request.respond(resp) {
        error!("响应失败 / Response failed: {}", e);
    }
}

/// Run the artifact manager HTTP server with async job execution.
///
/// This blocks the current thread (which becomes the execution thread).
/// A separate HTTP thread is spawned to handle requests.
///
/// 运行异步圣遗物管理 HTTP 服务器。
/// 当前线程成为执行线程，另起 HTTP 线程处理请求。
pub fn run_server(
    port: u16,
    ctrl: &mut GenshinGameController,
    manager: &ArtifactManager,
    enabled: Arc<AtomicBool>,
) -> Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    let server = Server::http(&addr)
        .map_err(|e| {
            let msg = format!("{}", e);
            if msg.contains("Address already in use") || msg.contains("address is already in use")
                || msg.contains("AddrInUse") || msg.contains("10048")
            {
                anyhow!(
                    "端口 {} 已被占用，请更换端口 / Port {} is already in use. \
                     Please choose a different port.",
                    port, port
                )
            } else {
                anyhow!(
                    "HTTP服务器启动失败 / HTTP server start failed on port {}: {}",
                    port, msg
                )
            }
        })?;

    info!(
        "HTTP服务器已启动：http://{} / HTTP server running at http://{}",
        addr, addr
    );

    // Shared state for async job tracking
    let job_state: Arc<Mutex<JobState>> = Arc::new(Mutex::new(JobState::idle()));

    // Channel for submitting jobs from HTTP thread to execution thread
    let (job_tx, job_rx) = mpsc::channel::<(String, ArtifactManageRequest)>();

    // Clone shared refs for the HTTP thread
    let http_state = job_state.clone();
    let http_enabled = enabled.clone();

    // Spawn HTTP handler thread
    let _http_thread = std::thread::spawn(move || {
        for request in server.incoming_requests() {
            let method = request.method().clone();
            let url = request.url().to_string();

            // --- Origin validation ---
            // Browser requests carry Origin; non-browser clients (curl) don't.
            // If Origin is present but not in the allowlist, reject with 403.
            // If absent, allow (CORS is a browser-enforced mechanism).
            let origin = get_origin(&request);
            let cors_origin: Option<String> = match &origin {
                Some(o) if is_origin_allowed(o) => Some(o.clone()),
                Some(o) => {
                    warn!("拒绝非法来源 / Rejected disallowed origin: {}", o);
                    respond_json(request, 403,
                        r#"{"error":"Origin not allowed"}"#, None);
                    continue;
                }
                None => None,
            };
            let cors_ref = cors_origin.as_deref();

            // CORS preflight (always respond for allowed origins)
            if method == Method::Options {
                let mut resp = Response::empty(204);
                if let Some(o) = cors_ref {
                    for header in cors_headers(o) {
                        resp.add_header(header);
                    }
                }
                let _ = request.respond(resp);
                continue;
            }

            match (method, url.as_str()) {
                (Method::Post, "/manage") => {
                    handle_manage(request, &http_enabled, &http_state, &job_tx, cors_ref);
                }

                // Lightweight poll — no result payload.
                // Returns state + jobId + progress (running) or summary (completed).
                (Method::Get, "/status") => {
                    let state = http_state.lock().unwrap();
                    let json = state.status_json();
                    drop(state);
                    respond_json(request, 200, &json, cors_ref);
                }

                // Full result — only available when completed.
                // Returns the complete ManageResult with per-instruction outcomes.
                (Method::Get, "/result") => {
                    let state = http_state.lock().unwrap();
                    match state.state {
                        JobPhase::Completed => {
                            if let Some(ref result) = state.result {
                                let json = serde_json::to_string(result).unwrap_or_else(|_| {
                                    r#"{"error":"序列化失败 / Serialization failed"}"#.to_string()
                                });
                                drop(state);
                                respond_json(request, 200, &json, cors_ref);
                            } else {
                                drop(state);
                                respond_json(request, 500,
                                    r#"{"error":"结果丢失 / Result data missing"}"#, cors_ref);
                            }
                        }
                        JobPhase::Running => {
                            drop(state);
                            respond_json(request, 409,
                                r#"{"error":"任务仍在执行 / Job still running. Poll GET /status."}"#,
                                cors_ref);
                        }
                        JobPhase::Idle => {
                            drop(state);
                            respond_json(request, 404,
                                r#"{"error":"没有已完成的任务 / No completed job available"}"#,
                                cors_ref);
                        }
                    }
                }

                // Health check — includes game window liveness.
                (Method::Get, "/health") => {
                    let is_enabled = http_enabled.load(Ordering::Relaxed);
                    let state = http_state.lock().unwrap();
                    let is_busy = state.state == JobPhase::Running;
                    drop(state);
                    let game_alive = is_game_window_alive();
                    let json = format!(
                        r#"{{"status":"ok","enabled":{},"busy":{},"gameAlive":{}}}"#,
                        is_enabled, is_busy, game_alive
                    );
                    respond_json(request, 200, &json, cors_ref);
                }

                _ => {
                    respond_json(request, 404, r#"{"error":"Not Found"}"#, cors_ref);
                }
            }
        }
    });

    // Block on channel — zero CPU when idle, wakes instantly on job arrival.
    // This thread owns ctrl (which is !Send) so it must be the original thread.
    info!("执行线程就绪 / Execution thread ready");
    while let Ok((job_id, request)) = job_rx.recv() {
        info!(
            "[job {}] 收到任务，1秒后开始执行 / Job received, starting in 1 second",
            job_id
        );

        // 1-second delay: let the client see the "running" state update
        // before the game window is focused and takes over the screen.
        yas::utils::sleep(1000);

        let progress_state = job_state.clone();
        let progress_fn = move |completed: usize, total: usize, current_id: &str, phase: &str| {
            if let Ok(mut state) = progress_state.lock() {
                state.progress = Some(JobProgress {
                    completed,
                    total,
                    current_id: current_id.to_string(),
                    phase: phase.to_string(),
                });
            }
        };

        let result = manager.execute(ctrl, request, Some(&progress_fn));

        {
            let mut state = job_state.lock().unwrap();
            *state = JobState::completed(job_id.clone(), result);
        }

        info!("[job {}] 执行完成 / Execution completed", job_id);
    }

    // Channel disconnected — HTTP thread exited
    info!("HTTP 线程已断开 / HTTP thread disconnected, shutting down");
    Ok(())
}

/// Handle POST /manage: validate origin, check busy, enforce size limit, submit job.
fn handle_manage(
    mut request: tiny_http::Request,
    enabled: &AtomicBool,
    state: &Arc<Mutex<JobState>>,
    job_tx: &mpsc::Sender<(String, ArtifactManageRequest)>,
    cors_origin: Option<&str>,
) {
    // Check if manager is enabled
    if !enabled.load(Ordering::Relaxed) {
        warn!("管理器已暂停，拒绝请求 / Manager paused, rejecting request");
        respond_json(
            request,
            503,
            r#"{"error":"管理器已暂停 / Manager is paused. Enable it in the GUI to accept requests."}"#,
            cors_origin,
        );
        return;
    }

    // Check if already busy
    {
        let s = state.lock().unwrap();
        if s.state == JobPhase::Running {
            respond_json(
                request,
                409,
                r#"{"error":"正在执行其他任务 / Another job is already running. Poll GET /status for progress."}"#,
                cors_origin,
            );
            return;
        }
    }

    // Enforce body size limit (Content-Length header)
    if let Some(len) = request.body_length() {
        if len > MAX_BODY_SIZE {
            respond_json(
                request,
                413,
                &format!(
                    r#"{{"error":"请求体过大（{} 字节，上限 {} 字节）/ Request body too large: {} bytes (max {})"}}"#,
                    len, MAX_BODY_SIZE, len, MAX_BODY_SIZE
                ),
                cors_origin,
            );
            return;
        }
    }

    // Read body
    let mut body = String::new();
    if let Err(e) = request.as_reader().read_to_string(&mut body) {
        respond_json(
            request,
            400,
            &format!(r#"{{"error":"读取请求体失败 / Failed to read body: {}"}}"#, e),
            cors_origin,
        );
        return;
    }

    // Enforce size limit for chunked transfers (no Content-Length)
    if body.len() > MAX_BODY_SIZE {
        respond_json(
            request,
            413,
            &format!(
                r#"{{"error":"请求体过大（{} 字节，上限 {} 字节）/ Request body too large: {} bytes (max {})"}}"#,
                body.len(), MAX_BODY_SIZE, body.len(), MAX_BODY_SIZE
            ),
            cors_origin,
        );
        return;
    }

    // Parse JSON
    let manage_request: ArtifactManageRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            respond_json(
                request,
                400,
                &format!(r#"{{"error":"JSON解析失败 / JSON parse error: {}"}}"#, e),
                cors_origin,
            );
            return;
        }
    };

    if manage_request.instructions.is_empty() {
        respond_json(
            request,
            400,
            r#"{"error":"指令列表为空 / Instructions list is empty"}"#,
            cors_origin,
        );
        return;
    }

    let total = manage_request.instructions.len();
    let job_id = uuid::Uuid::new_v4().to_string();

    info!(
        "[job {}] 收到 {} 条指令 / Received {} instructions",
        job_id, total, total
    );

    // Set state to Running
    {
        let mut s = state.lock().unwrap();
        *s = JobState::running(job_id.clone(), total);
    }

    // Send to execution thread
    if job_tx.send((job_id.clone(), manage_request)).is_err() {
        let mut s = state.lock().unwrap();
        *s = JobState::idle();
        respond_json(
            request,
            500,
            r#"{"error":"执行线程不可用 / Execution thread unavailable"}"#,
            cors_origin,
        );
        return;
    }

    // Return 202 Accepted immediately
    let json = format!(r#"{{"jobId":"{}","total":{}}}"#, job_id, total);
    respond_json(request, 202, &json, cors_origin);
}
