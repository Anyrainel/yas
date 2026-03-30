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
use crate::manager::orchestrator::ProgressFn;
use crate::scanner::common::models::GoodArtifact;

/// Abstraction over game interaction for testability.
pub trait ManageExecutor {
    fn execute(
        &mut self,
        request: ArtifactManageRequest,
        progress_fn: Option<&ProgressFn>,
    ) -> (ManageResult, Option<Vec<GoodArtifact>>);
}

/// Real executor: wraps a game controller and artifact manager.
pub struct GameExecutor {
    pub ctrl: GenshinGameController,
    pub manager: ArtifactManager,
}

impl ManageExecutor for GameExecutor {
    fn execute(
        &mut self,
        request: ArtifactManageRequest,
        progress_fn: Option<&ProgressFn>,
    ) -> (ManageResult, Option<Vec<GoodArtifact>>) {
        self.manager.execute(&mut self.ctrl, request, progress_fn)
    }
}

/// Maximum request body size (5 MB).
const MAX_BODY_SIZE: usize = 5 * 1024 * 1024;

/// State of the in-memory artifact inventory cache.
enum ArtifactCache {
    /// No scan has been performed yet.
    Empty,
    /// Last scan completed fully — data is available.
    Complete(Vec<crate::scanner::common::models::GoodArtifact>),
    /// Last scan was interrupted or incomplete — data is not reliable.
    Incomplete,
}

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
pub fn run_server<F>(
    port: u16,
    init_executor: F,
    enabled: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) -> Result<()>
where
    F: FnOnce() -> anyhow::Result<Box<dyn ManageExecutor>>,
{
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
    let server = Arc::new(server);

    info!(
        "HTTP服务器已启动：http://{} / HTTP server running at http://{}",
        addr, addr
    );

    // Shared state for async job tracking
    let job_state: Arc<Mutex<JobState>> = Arc::new(Mutex::new(JobState::idle()));

    // Latest artifact inventory state (populated by manager after backpack scan).
    let artifact_cache: Arc<Mutex<ArtifactCache>> =
        Arc::new(Mutex::new(ArtifactCache::Empty));

    // Channel for submitting jobs from HTTP thread to execution thread
    let (job_tx, job_rx) = mpsc::channel::<(String, ArtifactManageRequest)>();

    // Clone shared refs for the HTTP thread
    let http_state = job_state.clone();
    let http_enabled = enabled.clone();
    let http_artifact_cache = artifact_cache.clone();

    // Clone job_tx for the HTTP thread before moving the original
    let http_job_tx = job_tx.clone();

    // Spawn shutdown watcher: polls the flag and calls server.unblock()
    let shutdown_server = server.clone();
    let shutdown_flag = shutdown.clone();
    std::thread::spawn(move || {
        while !shutdown_flag.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        info!("收到关闭信号 / Shutdown signal received, stopping HTTP server");
        shutdown_server.unblock();
        // Drop the original sender so job_rx.recv() unblocks once the HTTP thread also exits
        drop(job_tx);
    });

    // Spawn HTTP handler thread
    let http_server = server.clone();
    let _http_thread = std::thread::spawn(move || {
        for request in http_server.incoming_requests() {
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
                    handle_manage(request, &http_enabled, &http_state, &http_job_tx, cors_ref);
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
                                    format!(r#"{{"error":"{}"}}"#, yas::lang::localize("序列化失败 / Serialization failed"))
                                });
                                drop(state);
                                respond_json(request, 200, &json, cors_ref);
                            } else {
                                drop(state);
                                respond_json(request, 500,
                                    &format!(r#"{{"error":"{}"}}"#, yas::lang::localize("结果丢失 / Result data missing")), cors_ref);
                            }
                        }
                        JobPhase::Running => {
                            drop(state);
                            respond_json(request, 409,
                                &format!(r#"{{"error":"{}"}}"#, yas::lang::localize("任务仍在执行 / Job still running. Poll GET /status.")),
                                cors_ref);
                        }
                        JobPhase::Idle => {
                            drop(state);
                            respond_json(request, 404,
                                &format!(r#"{{"error":"{}"}}"#, yas::lang::localize("没有已完成的任务 / No completed job available")),
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

                // Latest artifact inventory from the most recent complete scan.
                (Method::Get, "/artifacts") => {
                    let cache = http_artifact_cache.lock().unwrap();
                    match &*cache {
                        ArtifactCache::Complete(ref artifacts) => {
                            let json = serde_json::to_string(artifacts).unwrap_or_else(|_| {
                                format!(r#"{{"error":"{}"}}"#, yas::lang::localize("序列化失败 / Serialization failed"))
                            });
                            drop(cache);
                            respond_json(request, 200, &json, cors_ref);
                        }
                        ArtifactCache::Incomplete => {
                            drop(cache);
                            respond_json(request, 503,
                                &format!(r#"{{"error":"{}"}}"#, yas::lang::localize(
                                    "上次扫描未完成，数据不可用 / Last scan was incomplete. Data unavailable."
                                )),
                                cors_ref);
                        }
                        ArtifactCache::Empty => {
                            drop(cache);
                            respond_json(request, 404,
                                &format!(r#"{{"error":"{}"}}"#, yas::lang::localize(
                                    "没有可用的圣遗物数据，请先执行管理任务 / No artifact data available. Run a manage job first."
                                )),
                                cors_ref);
                        }
                    }
                }

                _ => {
                    respond_json(request, 404, r#"{"error":"Not Found"}"#, cors_ref);
                }
            }
        }
    });

    // Block on channel — zero CPU when idle, wakes instantly on job arrival.
    // This thread owns ctrl (which is !Send) so it must be the original thread.
    // Game controller + manager are created lazily on first job to avoid
    // focusing the game window at server startup.
    info!("执行线程就绪 / Execution thread ready");
    let mut executor: Option<Box<dyn ManageExecutor>> = None;
    let mut init_executor = Some(init_executor);

    while let Ok((job_id, request)) = job_rx.recv() {
        if shutdown.load(Ordering::Relaxed) {
            info!("[job {}] 服务器关闭中，跳过 / Server shutting down, skipping job", job_id);
            break;
        }
        info!(
            "[job {}] 收到任务，1秒后开始执行 / Job received, starting in 1 second",
            job_id
        );

        // 1-second delay: let the client see the "running" state update
        // before the game window is focused and takes over the screen.
        yas::utils::sleep(1000);

        // Lazy init: create executor on first job
        if executor.is_none() {
            if let Some(init_fn) = init_executor.take() {
                match init_fn() {
                    Ok(e) => {
                        executor = Some(e);
                    }
                    Err(e) => {
                        error!("游戏初始化失败 / Game init failed: {}", e);
                        let mut state = job_state.lock().unwrap();
                        let err_results: Vec<_> = request.instructions.iter().map(|i| {
                            crate::manager::models::InstructionResult {
                                id: i.id.clone(),
                                status: crate::manager::models::InstructionStatus::UiError,
                                detail: Some(format!(
                                    "游戏初始化失败 / Game init failed: {}", e
                                )),
                            }
                        }).collect();
                        let summary = crate::manager::models::ManageSummary::from_results(&err_results);
                        let result = crate::manager::models::ManageResult {
                            results: err_results,
                            summary,
                        };
                        *state = JobState::completed(job_id.clone(), result);
                        info!("[job {}] 执行失败（游戏初始化）/ Failed (game init)", job_id);
                        continue;
                    }
                }
            }
        }

        let exec = executor.as_mut().unwrap();

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

        let (result, artifact_snapshot) = exec.execute(request, Some(&progress_fn));

        // Update artifact cache based on scan completeness
        match artifact_snapshot {
            Some(snapshot) => {
                let count = snapshot.len();
                *artifact_cache.lock().unwrap() = ArtifactCache::Complete(snapshot);
                info!("[job {}] 圣遗物快照已更新（{} 个）/ Artifact snapshot updated ({} items)", job_id, count, count);
            }
            None => {
                // Scan was incomplete (interrupted, early stop, or no lock changes)
                // Mark as incomplete so GET /artifacts returns 503 instead of stale data
                let mut cache = artifact_cache.lock().unwrap();
                if !matches!(*cache, ArtifactCache::Empty) {
                    // Only mark incomplete if we previously had data — don't downgrade Empty
                    // if this was an equip-only job (no scan at all).
                    if result.results.iter().any(|r| r.status == InstructionStatus::Aborted) {
                        *cache = ArtifactCache::Incomplete;
                        info!("[job {}] 扫描被中断，圣遗物快照已失效 / Scan interrupted, artifact snapshot invalidated", job_id);
                    }
                }
            }
        }

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
            &format!(r#"{{"error":"{}"}}"#, yas::lang::localize("管理器已暂停 / Manager is paused. Enable it in the GUI to accept requests.")),
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
                &format!(r#"{{"error":"{}"}}"#, yas::lang::localize("正在执行其他任务 / Another job is already running. Poll GET /status for progress.")),
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
                &format!(r#"{{"error":"{}"}}"#, yas::lang::localize(&format!(
                    "请求体过大（{} 字节，上限 {} 字节）/ Request body too large: {} bytes (max {})",
                    len, MAX_BODY_SIZE, len, MAX_BODY_SIZE
                ))),
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
            &format!(r#"{{"error":"{}"}}"#, yas::lang::localize(&format!("读取请求体失败: {} / Failed to read body: {}", e, e))),
            cors_origin,
        );
        return;
    }

    // Enforce size limit for chunked transfers (no Content-Length)
    if body.len() > MAX_BODY_SIZE {
        respond_json(
            request,
            413,
            &format!(r#"{{"error":"{}"}}"#, yas::lang::localize(&format!(
                "请求体过大（{} 字节，上限 {} 字节）/ Request body too large: {} bytes (max {})",
                body.len(), MAX_BODY_SIZE, body.len(), MAX_BODY_SIZE
            ))),
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
                &format!(r#"{{"error":"{}"}}"#, yas::lang::localize(&format!("JSON解析失败: {} / JSON parse error: {}", e, e))),
                cors_origin,
            );
            return;
        }
    };

    if manage_request.instructions.is_empty() {
        respond_json(
            request,
            400,
            &format!(r#"{{"error":"{}"}}"#, yas::lang::localize("指令列表为空 / Instructions list is empty")),
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
            &format!(r#"{{"error":"{}"}}"#, yas::lang::localize("执行线程不可用 / Execution thread unavailable")),
            cors_origin,
        );
        return;
    }

    // Return 202 Accepted immediately
    let json = format!(r#"{{"jobId":"{}","total":{}}}"#, job_id, total);
    respond_json(request, 202, &json, cors_origin);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::common::models::{GoodArtifact, GoodSubStat};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct FakeExecutor {
        responses: Arc<Mutex<VecDeque<(ManageResult, Option<Vec<GoodArtifact>>)>>>,
        delay_ms: u64,
    }

    impl ManageExecutor for FakeExecutor {
        fn execute(
            &mut self,
            _request: ArtifactManageRequest,
            _progress_fn: Option<&ProgressFn>,
        ) -> (ManageResult, Option<Vec<GoodArtifact>>) {
            if self.delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(self.delay_ms));
            }
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("FakeExecutor: no more responses queued")
        }
    }

    fn make_result(statuses: &[(&str, InstructionStatus)]) -> ManageResult {
        let results: Vec<InstructionResult> = statuses
            .iter()
            .map(|(id, status)| InstructionResult {
                id: id.to_string(),
                status: status.clone(),
                detail: None,
            })
            .collect();
        let summary = ManageSummary::from_results(&results);
        ManageResult { results, summary }
    }

    fn make_artifact(set: &str, slot: &str, level: i32, locked: bool) -> GoodArtifact {
        GoodArtifact {
            set_key: set.to_string(),
            slot_key: slot.to_string(),
            rarity: 5,
            level,
            main_stat_key: "hp".to_string(),
            substats: vec![GoodSubStat {
                key: "critRate_".to_string(),
                value: 3.9,
                initial_value: None,
            }],
            location: String::new(),
            lock: locked,
            astral_mark: false,
            elixir_crafted: false,
            unactivated_substats: Vec::new(),
            total_rolls: None,
        }
    }

    fn make_manage_body(ids: &[&str]) -> String {
        let instructions: Vec<String> = ids
            .iter()
            .map(|id| {
                format!(
                    r#"{{"id":"{}","target":{{"setKey":"GladiatorsFinale","slotKey":"flower","rarity":5,"level":20,"mainStatKey":"hp","substats":[]}},"changes":{{"lock":true}}}}"#,
                    id
                )
            })
            .collect();
        format!(r#"{{"instructions":[{}]}}"#, instructions.join(","))
    }

    static NEXT_PORT: AtomicU16 = AtomicU16::new(19100);
    fn next_port() -> u16 {
        NEXT_PORT.fetch_add(1, Ordering::SeqCst)
    }

    fn start_test_server(
        responses: VecDeque<(ManageResult, Option<Vec<GoodArtifact>>)>,
        delay_ms: u64,
    ) -> (u16, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let port = next_port();
        let enabled = Arc::new(AtomicBool::new(true));
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let responses = Arc::new(Mutex::new(responses));
        let responses_clone = responses.clone();

        let handle = std::thread::spawn(move || {
            let init = move || -> anyhow::Result<Box<dyn ManageExecutor>> {
                Ok(Box::new(FakeExecutor {
                    responses: responses_clone,
                    delay_ms,
                }))
            };
            let _ = run_server(port, init, enabled, shutdown_clone);
        });

        let client = reqwest::blocking::Client::new();
        let url = format!("http://127.0.0.1:{}/health", port);
        for _ in 0..50 {
            if client.get(&url).send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        (port, shutdown, handle)
    }

    fn start_test_server_with_enabled(
        responses: VecDeque<(ManageResult, Option<Vec<GoodArtifact>>)>,
        delay_ms: u64,
        enabled: Arc<AtomicBool>,
    ) -> (u16, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let port = next_port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let responses = Arc::new(Mutex::new(responses));
        let responses_clone = responses.clone();

        let handle = std::thread::spawn(move || {
            let init = move || -> anyhow::Result<Box<dyn ManageExecutor>> {
                Ok(Box::new(FakeExecutor {
                    responses: responses_clone,
                    delay_ms,
                }))
            };
            let _ = run_server(port, init, enabled, shutdown_clone);
        });

        let client = reqwest::blocking::Client::new();
        let url = format!("http://127.0.0.1:{}/health", port);
        for _ in 0..50 {
            if client.get(&url).send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        (port, shutdown, handle)
    }

    fn stop_server(shutdown: &AtomicBool, handle: std::thread::JoinHandle<()>) {
        shutdown.store(true, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(300));
        let _ = handle.join();
    }

    /// Poll /status until `state == "completed"` or timeout.
    fn poll_until_completed(port: u16) {
        let client = reqwest::blocking::Client::new();
        let url = format!("http://127.0.0.1:{}/status", port);
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            let resp = client.get(&url).send().unwrap();
            let body: serde_json::Value = resp.json().unwrap();
            if body["state"] == "completed" {
                return;
            }
        }
        panic!("Job did not complete within timeout");
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_health_returns_ok_when_idle() {
        let responses = VecDeque::new();
        let (port, shutdown, handle) = start_test_server(responses, 0);

        let client = reqwest::blocking::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{}/health", port))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["status"], "ok");
        assert_eq!(body["enabled"], true);
        assert_eq!(body["busy"], false);

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_cors_allowed_origins() {
        let responses = VecDeque::new();
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}/health", port);

        // ggartifact.com
        let resp = client
            .get(&base)
            .header("Origin", "https://ggartifact.com")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let acao = resp
            .headers()
            .get("Access-Control-Allow-Origin")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(acao, "https://ggartifact.com");

        // localhost:3000
        let resp = client
            .get(&base)
            .header("Origin", "http://localhost:3000")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        // 127.0.0.1:5173
        let resp = client
            .get(&base)
            .header("Origin", "http://127.0.0.1:5173")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        // No Origin header
        let resp = client.get(&base).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_cors_disallowed_origin_returns_403() {
        let responses = VecDeque::new();
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();

        let resp = client
            .get(format!("http://127.0.0.1:{}/health", port))
            .header("Origin", "https://evil.com")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403);
        let body = resp.text().unwrap();
        assert!(body.contains("Origin not allowed"));

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_cors_preflight_options() {
        let responses = VecDeque::new();
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();

        let resp = client
            .request(
                reqwest::Method::OPTIONS,
                format!("http://127.0.0.1:{}/manage", port),
            )
            .header("Origin", "https://ggartifact.com")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 204);
        let acao = resp
            .headers()
            .get("Access-Control-Allow-Origin")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(acao, "https://ggartifact.com");

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_manage_empty_instructions_returns_400() {
        let responses = VecDeque::new();
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();

        let resp = client
            .post(format!("http://127.0.0.1:{}/manage", port))
            .header("Content-Type", "application/json")
            .body(r#"{"instructions":[]}"#)
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400);

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_manage_bad_json_returns_400() {
        let responses = VecDeque::new();
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();

        let resp = client
            .post(format!("http://127.0.0.1:{}/manage", port))
            .header("Content-Type", "application/json")
            .body("not json")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400);
        let body = resp.text().unwrap();
        assert!(body.contains("JSON"));

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_manage_accepts_valid_request() {
        let mut responses = VecDeque::new();
        responses.push_back((
            make_result(&[("a", InstructionStatus::Success)]),
            None,
        ));
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();

        let resp = client
            .post(format!("http://127.0.0.1:{}/manage", port))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 202);
        let body: serde_json::Value = resp.json().unwrap();
        assert!(body["jobId"].is_string());
        assert_eq!(body["total"], 1);

        poll_until_completed(port);
        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_manage_returns_503_when_disabled() {
        let responses = VecDeque::new();
        let enabled = Arc::new(AtomicBool::new(false));
        let (port, shutdown, handle) =
            start_test_server_with_enabled(responses, 0, enabled);
        let client = reqwest::blocking::Client::new();

        let resp = client
            .post(format!("http://127.0.0.1:{}/manage", port))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 503);

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_manage_returns_409_when_busy() {
        let mut responses = VecDeque::new();
        responses.push_back((
            make_result(&[("a", InstructionStatus::Success)]),
            None,
        ));
        // Second response in case first completes (shouldn't happen with 5s delay)
        responses.push_back((
            make_result(&[("b", InstructionStatus::Success)]),
            None,
        ));
        let (port, shutdown, handle) = start_test_server(responses, 5000);
        let client = reqwest::blocking::Client::new();

        // Submit first job
        let resp1 = client
            .post(format!("http://127.0.0.1:{}/manage", port))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send()
            .unwrap();
        assert_eq!(resp1.status().as_u16(), 202);

        // Wait for the job to start processing
        std::thread::sleep(Duration::from_millis(500));

        // Submit second job — should be 409
        let resp2 = client
            .post(format!("http://127.0.0.1:{}/manage", port))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["b"]))
            .send()
            .unwrap();
        assert_eq!(resp2.status().as_u16(), 409);

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_health_shows_busy_during_job() {
        let mut responses = VecDeque::new();
        responses.push_back((
            make_result(&[("a", InstructionStatus::Success)]),
            None,
        ));
        let (port, shutdown, handle) = start_test_server(responses, 5000);
        let client = reqwest::blocking::Client::new();

        // Submit a job
        client
            .post(format!("http://127.0.0.1:{}/manage", port))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send()
            .unwrap();

        std::thread::sleep(Duration::from_millis(500));

        let resp = client
            .get(format!("http://127.0.0.1:{}/health", port))
            .send()
            .unwrap();
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["busy"], true);

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_status_idle_before_any_job() {
        let responses = VecDeque::new();
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();

        let resp = client
            .get(format!("http://127.0.0.1:{}/status", port))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["state"], "idle");

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_result_returns_404_when_idle() {
        let responses = VecDeque::new();
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();

        let resp = client
            .get(format!("http://127.0.0.1:{}/result", port))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_result_returns_409_when_running() {
        let mut responses = VecDeque::new();
        responses.push_back((
            make_result(&[("a", InstructionStatus::Success)]),
            None,
        ));
        let (port, shutdown, handle) = start_test_server(responses, 5000);
        let client = reqwest::blocking::Client::new();

        client
            .post(format!("http://127.0.0.1:{}/manage", port))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send()
            .unwrap();

        std::thread::sleep(Duration::from_millis(500));

        let resp = client
            .get(format!("http://127.0.0.1:{}/result", port))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 409);

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_full_lifecycle_submit_poll_result() {
        let mut responses = VecDeque::new();
        responses.push_back((
            make_result(&[
                ("i1", InstructionStatus::Success),
                ("i2", InstructionStatus::NotFound),
                ("i3", InstructionStatus::AlreadyCorrect),
            ]),
            None,
        ));
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}", port);

        // Submit
        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["i1", "i2", "i3"]))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 202);

        // Poll until completed
        poll_until_completed(port);

        // Check status summary
        let resp = client.get(format!("{}/status", base)).send().unwrap();
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["state"], "completed");
        assert_eq!(body["summary"]["total"], 3);
        assert_eq!(body["summary"]["success"], 1);
        assert_eq!(body["summary"]["not_found"], 1);
        assert_eq!(body["summary"]["already_correct"], 1);

        // Get full result
        let resp = client.get(format!("{}/result", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["results"][0]["id"], "i1");
        assert_eq!(body["results"][0]["status"], "success");
        assert_eq!(body["results"][1]["id"], "i2");
        assert_eq!(body["results"][1]["status"], "not_found");
        assert_eq!(body["results"][2]["id"], "i3");
        assert_eq!(body["results"][2]["status"], "already_correct");

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_artifacts_returns_404_before_any_scan() {
        let responses = VecDeque::new();
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();

        let resp = client
            .get(format!("http://127.0.0.1:{}/artifacts", port))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_artifacts_returns_200_after_complete_scan() {
        let mut responses = VecDeque::new();
        let artifacts = vec![
            make_artifact("GladiatorsFinale", "flower", 20, true),
            make_artifact("WanderersTroupe", "plume", 16, false),
        ];
        responses.push_back((
            make_result(&[("a", InstructionStatus::Success)]),
            Some(artifacts),
        ));
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}", port);

        // Submit and wait
        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        // Check artifacts
        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert!(body.is_array());
        assert_eq!(body.as_array().unwrap().len(), 2);
        assert_eq!(body[0]["setKey"], "GladiatorsFinale");
        assert_eq!(body[1]["setKey"], "WanderersTroupe");

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_artifacts_stays_404_after_no_snapshot_job() {
        let mut responses = VecDeque::new();
        responses.push_back((
            make_result(&[("a", InstructionStatus::Success)]),
            None,
        ));
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}", port);

        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_artifacts_returns_503_after_aborted_scan_invalidates_cache() {
        let mut responses = VecDeque::new();
        // Job 1: complete scan with snapshot
        let artifacts = vec![make_artifact("GladiatorsFinale", "flower", 20, true)];
        responses.push_back((
            make_result(&[("a", InstructionStatus::Success)]),
            Some(artifacts),
        ));
        // Job 2: aborted, no snapshot
        responses.push_back((
            make_result(&[("b", InstructionStatus::Aborted)]),
            None,
        ));
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}", port);

        // Job 1
        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        // Verify cache is populated
        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        // Job 2 (aborted)
        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["b"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        // Cache should now be 503 (Incomplete)
        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 503);

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_game_init_failure_produces_ui_error_results() {
        let port = next_port();
        let enabled = Arc::new(AtomicBool::new(true));
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();

        let handle = std::thread::spawn(move || {
            let init = move || -> anyhow::Result<Box<dyn ManageExecutor>> {
                Err(anyhow::anyhow!("Game window not found"))
            };
            let _ = run_server(port, init, enabled, shutdown_clone);
        });

        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}", port);
        for _ in 0..50 {
            if client.get(format!("{}/health", base)).send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        // Submit job
        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["x", "y"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        // Check result
        let resp = client.get(format!("{}/result", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        let results = body["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["status"], "ui_error");
        assert_eq!(results[1]["status"], "ui_error");

        shutdown.store(true, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(300));
        let _ = handle.join();
    }

    #[test]
    fn test_unknown_route_returns_404() {
        let responses = VecDeque::new();
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();

        let resp = client
            .get(format!("http://127.0.0.1:{}/nonexistent", port))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        stop_server(&shutdown, handle);
    }

    #[test]
    fn test_sequential_jobs_reset_state() {
        let mut responses = VecDeque::new();
        responses.push_back((
            make_result(&[("j1", InstructionStatus::Success)]),
            None,
        ));
        responses.push_back((
            make_result(&[("j2", InstructionStatus::NotFound)]),
            None,
        ));
        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}", port);

        // Job 1
        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["j1"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        let resp = client.get(format!("{}/result", base)).send().unwrap();
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["results"][0]["id"], "j1");
        assert_eq!(body["results"][0]["status"], "success");

        // Job 2
        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["j2"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        let resp = client.get(format!("{}/result", base)).send().unwrap();
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["results"][0]["id"], "j2");
        assert_eq!(body["results"][0]["status"], "not_found");

        stop_server(&shutdown, handle);
    }
}
