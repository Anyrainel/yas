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
use yas::{log_debug, log_error, log_info, log_warn};
use tiny_http::{Header, Method, Response, Server};

use crate::manager::models::*;
use crate::manager::orchestrator::ArtifactManager;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::manager::orchestrator::ProgressFn;
use crate::scanner::common::models::GoodArtifact;

// ================================================================
// File logging: saves request bodies as JSON for replay/debugging
// ================================================================

/// Format a timestamp string from SystemTime (local time approximation via UNIX epoch offset).
fn timestamp_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs();
    let millis = dur.subsec_millis();
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{:02}-{:02}-{:02}_{:03}", h, m, s, millis)
}

/// Save a request body as a timestamped JSON file in the log/ directory.
fn save_request(endpoint: &str, body: &str) {
    let log_dir = std::path::PathBuf::from("log");
    if std::fs::create_dir_all(&log_dir).is_err() {
        return;
    }
    let ts = timestamp_string();
    let filename = format!("{}_{}.json", endpoint, ts);
    let path = log_dir.join(&filename);
    if let Err(e) = std::fs::write(&path, body) {
        log_error!("保存请求失败: {}: {}", "Failed to save request {}: {}", filename, e);
    }
}

/// Job types that can be submitted to the execution thread.
enum JobRequest {
    Manage(LockManageRequest),
    Equip(EquipRequest),
}

/// Abstraction over game interaction for testability.
pub trait ManageExecutor {
    fn execute(
        &mut self,
        request: LockManageRequest,
        progress_fn: Option<&ProgressFn>,
        cancel_token: yas::cancel::CancelToken,
    ) -> (ManageResult, Option<Vec<GoodArtifact>>);

    fn execute_equip(
        &mut self,
        request: EquipRequest,
        progress_fn: Option<&ProgressFn>,
        cancel_token: yas::cancel::CancelToken,
    ) -> ManageResult;
}

/// Real executor: wraps a game controller and artifact manager.
pub struct GameExecutor {
    pub ctrl: GenshinGameController,
    pub manager: ArtifactManager,
}

impl ManageExecutor for GameExecutor {
    fn execute(
        &mut self,
        request: LockManageRequest,
        progress_fn: Option<&ProgressFn>,
        cancel_token: yas::cancel::CancelToken,
    ) -> (ManageResult, Option<Vec<GoodArtifact>>) {
        self.manager.execute(&mut self.ctrl, request, progress_fn, cancel_token)
    }

    fn execute_equip(
        &mut self,
        request: EquipRequest,
        progress_fn: Option<&ProgressFn>,
        cancel_token: yas::cancel::CancelToken,
    ) -> ManageResult {
        self.manager.execute_equip(&mut self.ctrl, request, progress_fn, cancel_token)
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
    "http://ggartifact.com",
];

/// Check if an origin is allowed.
///
/// Allows:
/// - `https://ggartifact.com` (production)
/// - `http://localhost[:port]` (development)
/// - `http://127.0.0.1[:port]` (development)
fn is_origin_allowed(origin: &str) -> bool {
    let origin = origin.trim_end_matches('/');
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
        Header::from_bytes("Access-Control-Allow-Private-Network", "true").unwrap(),
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
        log_error!("响应失败: {}", "Response failed: {}", e);
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

    log_info!(
        "HTTP服务器已启动：http://{}",
        "HTTP server running at http://{}",
        addr
    );

    // Shared state for async job tracking
    let job_state: Arc<Mutex<JobState>> = Arc::new(Mutex::new(JobState::idle()));

    // Latest artifact inventory state (populated by manager after backpack scan).
    let artifact_cache: Arc<Mutex<ArtifactCache>> =
        Arc::new(Mutex::new(ArtifactCache::Empty));

    // Channel for submitting jobs from HTTP thread to execution thread
    let (job_tx, job_rx) = mpsc::channel::<(String, JobRequest)>();

    // Clone shared refs for the HTTP thread
    let http_state = job_state.clone();
    let http_enabled = enabled.clone();
    let http_artifact_cache = artifact_cache.clone();

    // Clone job_tx for the HTTP thread before moving the original
    let http_job_tx = job_tx.clone();

    // Spawn shutdown watcher: polls the flag and calls server.unblock()
    let shutdown_server = server.clone();
    let shutdown_flag = shutdown.clone();
    let shutdown_watcher = std::thread::spawn(move || {
        while !shutdown_flag.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        log_info!("收到关闭信号，停止HTTP服务器", "Shutdown signal received, stopping HTTP server");
        shutdown_server.unblock();
        // Drop the original sender so job_rx.recv() unblocks once the HTTP thread also exits
        drop(job_tx);
    });

    // Spawn HTTP handler thread
    let http_server = server.clone();
    let http_thread = std::thread::spawn(move || {
        for request in http_server.incoming_requests() {
            let method = request.method().clone();
            let url = request.url().to_string();

            // --- Origin validation ---
            // Browser requests carry Origin; non-browser clients (curl) don't.
            // If Origin is present but not in the allowlist, reject with 403.
            // If absent, allow (CORS is a browser-enforced mechanism).
            let origin = get_origin(&request);
            let cors_origin: Option<String> = match &origin {
                Some(o) if is_origin_allowed(o) => {
                    Some(o.trim_end_matches('/').to_string())
                }
                Some(o) => {
                    log_warn!("拒绝非法来源: {}", "Rejected disallowed origin: {}", o);
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
                if let Err(e) = request.respond(resp) {
                    log_debug!("CORS preflight 响应失败: {}", "CORS preflight response failed: {}", e);
                }
                continue;
            }

            match (method, url.as_str()) {
                (Method::Post, "/manage") => {
                    handle_manage(request, &http_enabled, &http_state, &http_job_tx, cors_ref);
                }

                (Method::Post, "/equip") => {
                    handle_equip(request, &http_enabled, &http_state, &http_job_tx, cors_ref);
                }

                // Lightweight poll — no result payload.
                // Returns state + jobId + progress (running) or summary (completed).
                (Method::Get, "/status") => {
                    let state = http_state.lock().unwrap();
                    let json = state.status_json();
                    drop(state);
                    respond_json(request, 200, &json, cors_ref);
                }

                // Full result — requires jobId query param, idempotent.
                (Method::Get, url) if url.starts_with("/result") => {
                    // Parse jobId from query string: /result?jobId=xxx
                    let query_job_id = url.split('?')
                        .nth(1)
                        .and_then(|qs| qs.split('&').find(|p| p.starts_with("jobId=")))
                        .map(|p| &p[6..]);

                    match query_job_id {
                        None | Some("") => {
                            respond_json(request, 400,
                                r#"{"error":"missing required query parameter: jobId"}"#, cors_ref);
                        }
                        Some(requested_id) => {
                            let state = http_state.lock().unwrap();
                            match state.state {
                                JobPhase::Completed => {
                                    let actual_id = state.job_id.as_deref().unwrap_or("");
                                    if actual_id != requested_id {
                                        drop(state);
                                        respond_json(request, 404,
                                            r#"{"error":"job not found"}"#, cors_ref);
                                    } else if let Some(ref result) = state.result {
                                        let json = serde_json::to_string(result).unwrap_or_else(|_| {
                                            r#"{"error":"serialization failed"}"#.to_string()
                                        });
                                        drop(state);
                                        respond_json(request, 200, &json, cors_ref);
                                    } else {
                                        drop(state);
                                        respond_json(request, 500,
                                            r#"{"error":"result data missing"}"#, cors_ref);
                                    }
                                }
                                JobPhase::Running => {
                                    let actual_id = state.job_id.as_deref().unwrap_or("");
                                    if actual_id != requested_id {
                                        drop(state);
                                        respond_json(request, 404,
                                            r#"{"error":"job not found"}"#, cors_ref);
                                    } else {
                                        drop(state);
                                        respond_json(request, 409,
                                            r#"{"error":"job still running"}"#, cors_ref);
                                    }
                                }
                                JobPhase::Idle => {
                                    drop(state);
                                    respond_json(request, 404,
                                        r#"{"error":"job not found"}"#, cors_ref);
                                }
                            }
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
    log_debug!("执行线程就绪", "Execution thread ready");
    let mut executor: Option<Box<dyn ManageExecutor>> = None;
    let mut init_executor = Some(init_executor);

    while let Ok((job_id, request)) = job_rx.recv() {
        if shutdown.load(Ordering::Relaxed) {
            log_info!("[job {}] 服务器关闭中，跳过", "[job {}] Server shutting down, skipping job", job_id);
            break;
        }
        log_info!(
            "[job {}] 收到任务，1秒后开始执行",
            "[job {}] Job received, starting in 1 second",
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
                        log_error!(
                            "[job {}] 游戏初始化失败:\n{:#}",
                            "[job {}] Game init failed:\n{:#}",
                            job_id, e
                        );
                        let mut state = job_state.lock().unwrap();
                        let total_count = match &request {
                            JobRequest::Manage(r) => r.lock.len() + r.unlock.len(),
                            JobRequest::Equip(r) => r.equip.len(),
                        };
                        let err_results: Vec<_> = (0..total_count).map(|idx| {
                            crate::manager::models::InstructionResult {
                                id: format!("item_{}", idx),
                                status: crate::manager::models::InstructionStatus::UiError,
                            }
                        }).collect();
                        let summary = crate::manager::models::ManageSummary::from_results(&err_results);
                        let result = crate::manager::models::ManageResult {
                            results: err_results,
                            summary,
                        };
                        *state = JobState::completed(job_id.clone(), result);
                        continue;
                    }
                }
            }
        }

        let exec = executor.as_mut().unwrap();

        // Immediately invalidate any cached artifact snapshot before execution
        // starts. Lock/unlock changes and equip changes will modify in-game
        // state, so clients must not read stale data during the scan.
        {
            let invalidate_now = match &request {
                JobRequest::Manage(r) => !r.lock.is_empty() || !r.unlock.is_empty(),
                JobRequest::Equip(_) => true,
            };
            if invalidate_now {
                let mut cache = artifact_cache.lock().unwrap();
                if matches!(*cache, ArtifactCache::Complete(_)) {
                    *cache = ArtifactCache::Incomplete;
                    log_debug!("[job {}] 执行前清除快照缓存", "[job {}] Pre-execution: artifact cache invalidated", job_id);
                }
            }
        }

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

        let cancel_token = yas::cancel::CancelToken::new();
        let (result, artifact_snapshot, invalidates_cache) = match std::panic::catch_unwind(
            std::panic::AssertUnwindSafe(|| match request {
                JobRequest::Manage(manage_req) => {
                    let has_lock = !manage_req.lock.is_empty() || !manage_req.unlock.is_empty();
                    let (result, snapshot) = exec.execute(manage_req, Some(&progress_fn), cancel_token);
                    (result, snapshot, has_lock)
                }
                JobRequest::Equip(equip_req) => {
                    let result = exec.execute_equip(equip_req, Some(&progress_fn), cancel_token);
                    // Equip jobs always invalidate cache — in-game equipment state changed.
                    (result, None, true)
                }
            })
        ) {
            Ok(r) => r,
            Err(panic_info) => {
                let msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else {
                    "unknown panic".to_string()
                };
                log_error!("[job {}] 执行时发生panic: {}", "[job {}] Panic during execution: {}", job_id, msg);
                let summary = ManageSummary {
                    total: 0, success: 0, already_correct: 0, not_found: 0,
                    errors: 1, aborted: 0,
                };
                let result = ManageResult { results: Vec::new(), summary };
                *job_state.lock().unwrap() = JobState::completed(job_id.clone(), result);
                continue;
            }
        };

        // Update artifact cache based on scan completeness
        match artifact_snapshot {
            Some(snapshot) => {
                let count = snapshot.len();
                *artifact_cache.lock().unwrap() = ArtifactCache::Complete(snapshot);
                log_info!("[job {}] 圣遗物快照已更新（{} 个）", "[job {}] Artifact snapshot updated ({} items)", job_id, count);
            }
            None => {
                // No snapshot returned — scan was incomplete, equip-only, or interrupted.
                // If this job modified in-game state, any cached snapshot is now stale.
                if invalidates_cache {
                    let mut cache = artifact_cache.lock().unwrap();
                    if matches!(*cache, ArtifactCache::Complete(_)) {
                        *cache = ArtifactCache::Incomplete;
                        log_info!("[job {}] 游戏内状态已变更，快照已失效", "[job {}] In-game state changed, artifact snapshot invalidated", job_id);
                    }
                }
            }
        }

        {
            let mut state = job_state.lock().unwrap();
            *state = JobState::completed(job_id.clone(), result);
        }

        log_info!("[job {}] 执行完成", "[job {}] Execution completed", job_id);
    }

    // Channel disconnected — wait for internal threads to fully stop before
    // returning. Without this, detached threads may still be tearing down
    // when the process exits, causing heap corruption in test suites.
    log_debug!("执行线程退出，等待内部线程", "Execution loop exited, joining internal threads");
    let _ = shutdown_watcher.join();
    let _ = http_thread.join();
    log_debug!("HTTP 服务器已完全关闭", "HTTP server fully shut down");
    Ok(())
}

/// Validate a single artifact entry. Returns `Some(message)` on failure.
fn validate_artifact(artifact: &crate::scanner::common::models::GoodArtifact) -> Option<String> {
    if artifact.set_key.trim().is_empty() {
        return Some("empty setKey".to_string());
    }
    if artifact.slot_key.trim().is_empty() {
        return Some("empty slotKey".to_string());
    }
    if artifact.main_stat_key.trim().is_empty() {
        return Some("empty mainStatKey".to_string());
    }
    if artifact.rarity < 4 || artifact.rarity > 5 {
        return Some(format!("invalid rarity: {} (must be 4-5)", artifact.rarity));
    }
    if artifact.level < 0 || artifact.level > 20 {
        return Some(format!("invalid level: {} (must be 0-20)", artifact.level));
    }
    None
}

/// Handle POST /manage: validate origin, check busy, enforce size limit, submit job.
fn handle_manage(
    mut request: tiny_http::Request,
    enabled: &AtomicBool,
    state: &Arc<Mutex<JobState>>,
    job_tx: &mpsc::Sender<(String, JobRequest)>,
    cors_origin: Option<&str>,
) {
    // Check if manager is enabled
    if !enabled.load(Ordering::Relaxed) {
        log_warn!("管理器已暂停，拒绝请求", "Manager paused, rejecting request");
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

    // Log request body to file
    save_request("manage", &body);

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
    let manage_request: LockManageRequest = match serde_json::from_str(&body) {
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

    if manage_request.lock.is_empty() && manage_request.unlock.is_empty() {
        respond_json(
            request,
            400,
            &format!(r#"{{"error":"{}"}}"#, yas::lang::localize("lock 和 unlock 列表均为空 / Both lock and unlock lists are empty")),
            cors_origin,
        );
        return;
    }

    // Validate ALL entries upfront — reject the whole request on any invalid entry.
    for (list_name, artifacts) in [("lock", &manage_request.lock), ("unlock", &manage_request.unlock)] {
        for (idx, artifact) in artifacts.iter().enumerate() {
            if let Some(err) = validate_artifact(artifact) {
                respond_json(
                    request,
                    400,
                    &format!(r#"{{"error":"{}[{}]: {}"}}"#, list_name, idx, err),
                    cors_origin,
                );
                return;
            }
        }
    }

    let total = manage_request.lock.len() + manage_request.unlock.len();
    let job_id = uuid::Uuid::new_v4().to_string();

    log_info!(
        "[job {}] 收到 {} 条管理请求（lock: {}, unlock: {}）",
        "[job {}] Received {} manage items (lock: {}, unlock: {})",
        job_id, total, manage_request.lock.len(), manage_request.unlock.len()
    );

    // Set state to Running
    {
        let mut s = state.lock().unwrap();
        *s = JobState::running(job_id.clone(), total);
    }

    // Send to execution thread
    if job_tx.send((job_id.clone(), JobRequest::Manage(manage_request))).is_err() {
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

/// Handle POST /equip: validate, parse EquipRequest, submit job.
fn handle_equip(
    mut request: tiny_http::Request,
    enabled: &AtomicBool,
    state: &Arc<Mutex<JobState>>,
    job_tx: &mpsc::Sender<(String, JobRequest)>,
    cors_origin: Option<&str>,
) {
    if !enabled.load(Ordering::Relaxed) {
        log_warn!("管理器已暂停，拒绝请求", "Manager paused, rejecting request");
        respond_json(
            request,
            503,
            &format!(r#"{{"error":"{}"}}"#, yas::lang::localize("管理器已暂停 / Manager is paused. Enable it in the GUI to accept requests.")),
            cors_origin,
        );
        return;
    }

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

    // Log request body to file
    save_request("equip", &body);

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

    let equip_request: EquipRequest = match serde_json::from_str(&body) {
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

    if equip_request.equip.is_empty() {
        respond_json(
            request,
            400,
            &format!(r#"{{"error":"{}"}}"#, yas::lang::localize("equip 列表为空 / Equip list is empty")),
            cors_origin,
        );
        return;
    }

    // Validate all artifact entries
    for (idx, instr) in equip_request.equip.iter().enumerate() {
        if let Some(err) = validate_artifact(&instr.artifact) {
            respond_json(
                request,
                400,
                &format!(r#"{{"error":"equip[{}]: {}"}}"#, idx, err),
                cors_origin,
            );
            return;
        }
    }

    let total = equip_request.equip.len();
    let job_id = uuid::Uuid::new_v4().to_string();

    log_info!(
        "[job {}] 收到 {} 条装备请求",
        "[job {}] Received {} equip instructions",
        job_id, total
    );

    {
        let mut s = state.lock().unwrap();
        *s = JobState::running(job_id.clone(), total);
    }

    if job_tx.send((job_id.clone(), JobRequest::Equip(equip_request))).is_err() {
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

    // Serialize all server tests to prevent concurrent tiny_http teardown,
    // which causes STATUS_HEAP_CORRUPTION on Windows.
    static SERVER_LOCK: Mutex<()> = Mutex::new(());

    struct FakeExecutor {
        responses: Arc<Mutex<VecDeque<(ManageResult, Option<Vec<GoodArtifact>>)>>>,
        delay_ms: u64,
    }

    impl ManageExecutor for FakeExecutor {
        fn execute(
            &mut self,
            _request: LockManageRequest,
            _progress_fn: Option<&ProgressFn>,
            _cancel_token: yas::cancel::CancelToken,
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

        fn execute_equip(
            &mut self,
            _request: EquipRequest,
            _progress_fn: Option<&ProgressFn>,
            _cancel_token: yas::cancel::CancelToken,
        ) -> ManageResult {
            let results = Vec::new();
            let summary = ManageSummary::from_results(&results);
            ManageResult { results, summary }
        }
    }

    fn make_result(statuses: &[(&str, InstructionStatus)]) -> ManageResult {
        let results: Vec<InstructionResult> = statuses
            .iter()
            .map(|(id, status)| InstructionResult {
                id: id.to_string(),
                status: status.clone(),
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
        let artifacts: Vec<String> = ids
            .iter()
            .map(|_id| {
                r#"{"setKey":"GladiatorsFinale","slotKey":"flower","rarity":5,"level":20,"mainStatKey":"hp","substats":[],"location":"","lock":false,"astralMark":false,"elixirCrafted":false,"unactivatedSubstats":[]}"#.to_string()
            })
            .collect();
        format!(r#"{{"lock":[{}]}}"#, artifacts.join(","))
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
    // Tests: consolidated from 13 → 5 to minimize server instances.
    // All tests acquire SERVER_LOCK to run sequentially.
    // -----------------------------------------------------------------------

    /// Read-only endpoints + basic submit/lifecycle + artifacts + sequential jobs.
    /// Consolidates: test_readonly_endpoints, test_manage_accepts_valid_request,
    /// test_full_lifecycle_submit_poll_result, test_artifacts_returns_200_after_complete_scan,
    /// test_artifacts_stays_404_after_no_snapshot_job, test_sequential_jobs_reset_state.
    #[test]
    fn test_standard_flow() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let mut responses = VecDeque::new();
        // Job 1: single item, no snapshot (tests accept + artifacts 404)
        responses.push_back((
            make_result(&[("a", InstructionStatus::Success)]),
            None,
        ));
        // Job 2: 3 items, no snapshot (tests full lifecycle)
        responses.push_back((
            make_result(&[
                ("i1", InstructionStatus::Success),
                ("i2", InstructionStatus::NotFound),
                ("i3", InstructionStatus::AlreadyCorrect),
            ]),
            None,
        ));
        // Job 3: with snapshot (tests artifacts 200)
        let artifacts = vec![
            make_artifact("GladiatorsFinale", "flower", 20, true),
            make_artifact("WanderersTroupe", "plume", 16, false),
        ];
        responses.push_back((
            make_result(&[("art1", InstructionStatus::Success)]),
            Some(artifacts),
        ));
        // Jobs 4-5: sequential jobs (tests state reset)
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

        // === Read-only checks (no jobs submitted yet) ===

        // health returns ok when idle
        let resp = client.get(format!("{}/health", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["status"], "ok");
        assert_eq!(body["enabled"], true);
        assert_eq!(body["busy"], false);

        // CORS: allowed origins
        let resp = client
            .get(format!("{}/health", base))
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

        let resp = client
            .get(format!("{}/health", base))
            .header("Origin", "http://localhost:3000")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        let resp = client
            .get(format!("{}/health", base))
            .header("Origin", "http://127.0.0.1:5173")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        let resp = client.get(format!("{}/health", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        // CORS: disallowed origin returns 403
        let resp = client
            .get(format!("{}/health", base))
            .header("Origin", "https://evil.com")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403);
        let body = resp.text().unwrap();
        assert!(body.contains("Origin not allowed"));

        // CORS: preflight OPTIONS
        let resp = client
            .request(
                reqwest::Method::OPTIONS,
                format!("{}/manage", base),
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

        // manage: empty instructions returns 400
        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(r#"{"lock":[],"unlock":[]}"#)
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400);

        // manage: bad JSON returns 400
        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body("not json")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400);
        let body = resp.text().unwrap();
        assert!(body.contains("JSON"));

        // status: idle before any job
        let resp = client.get(format!("{}/status", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["state"], "idle");

        // result: 400 without jobId
        let resp = client.get(format!("{}/result", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 400);

        // result: 404 for unknown jobId
        let resp = client
            .get(format!("{}/result?jobId=nonexistent", base))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        // unknown route returns 404
        let resp = client
            .get(format!("{}/nonexistent", base))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        // artifacts: 404 before any scan
        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        // === Job 1: basic accept + artifacts stays 404 ===

        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 202);
        let body: serde_json::Value = resp.json().unwrap();
        assert!(body["jobId"].is_string());
        assert_eq!(body["total"], 1);

        poll_until_completed(port);

        // No snapshot → artifacts stays 404
        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        // === Job 2: full lifecycle (submit/poll/result) ===

        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["i1", "i2", "i3"]))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 202);
        let submit_body: serde_json::Value = resp.json().unwrap();
        let job_id = submit_body["jobId"].as_str().unwrap().to_string();

        poll_until_completed(port);

        // Check status summary
        let resp = client.get(format!("{}/status", base)).send().unwrap();
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["state"], "completed");
        assert_eq!(body["summary"]["total"], 3);
        assert_eq!(body["summary"]["success"], 1);
        assert_eq!(body["summary"]["not_found"], 1);
        assert_eq!(body["summary"]["already_correct"], 1);

        // Get full result (with jobId)
        let resp = client.get(format!("{}/result?jobId={}", base, job_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["results"][0]["id"], "i1");
        assert_eq!(body["results"][0]["status"], "success");
        assert_eq!(body["results"][1]["id"], "i2");
        assert_eq!(body["results"][1]["status"], "not_found");
        assert_eq!(body["results"][2]["id"], "i3");
        assert_eq!(body["results"][2]["status"], "already_correct");

        // Result is idempotent
        let resp = client.get(format!("{}/result?jobId={}", base, job_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        // === Job 3: artifacts snapshot ===

        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["art1"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert!(body.is_array());
        assert_eq!(body.as_array().unwrap().len(), 2);
        assert_eq!(body[0]["setKey"], "GladiatorsFinale");
        assert_eq!(body[1]["setKey"], "WanderersTroupe");

        // === Jobs 4-5: sequential jobs reset state ===

        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["j1"]))
            .send()
            .unwrap();
        let job1_id = resp.json::<serde_json::Value>().unwrap()["jobId"]
            .as_str().unwrap().to_string();
        poll_until_completed(port);

        let resp = client.get(format!("{}/result?jobId={}", base, job1_id)).send().unwrap();
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["results"][0]["id"], "j1");
        assert_eq!(body["results"][0]["status"], "success");

        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["j2"]))
            .send()
            .unwrap();
        let job2_id = resp.json::<serde_json::Value>().unwrap()["jobId"]
            .as_str().unwrap().to_string();
        poll_until_completed(port);

        let resp = client.get(format!("{}/result?jobId={}", base, job2_id)).send().unwrap();
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["results"][0]["id"], "j2");
        assert_eq!(body["results"][0]["status"], "not_found");

        // Job 1's result is gone — replaced by job 2
        let resp = client.get(format!("{}/result?jobId={}", base, job1_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        stop_server(&shutdown, handle);
    }

    /// Artifact cache invalidation across multiple job patterns.
    /// Consolidates: test_artifacts_returns_503_after_aborted_scan_invalidates_cache,
    /// test_artifacts_invalidated_when_lock_job_returns_no_snapshot,
    /// test_artifacts_cleared_when_update_inventory_off_after_on.
    #[test]
    fn test_artifact_cache_invalidation() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let mut responses = VecDeque::new();
        // Pair 1: populate → aborted invalidates → 503
        responses.push_back((
            make_result(&[("a", InstructionStatus::Success)]),
            Some(vec![make_artifact("GladiatorsFinale", "flower", 20, true)]),
        ));
        responses.push_back((
            make_result(&[("b", InstructionStatus::Aborted)]),
            None,
        ));
        // Pair 2: populate → success no snapshot (stop_on_all_matched) → 503
        responses.push_back((
            make_result(&[("c", InstructionStatus::Success)]),
            Some(vec![make_artifact("GladiatorsFinale", "flower", 20, true)]),
        ));
        responses.push_back((
            make_result(&[("d", InstructionStatus::Success)]),
            None,
        ));
        // Pair 3: populate with 2 items → success no snapshot (update_inv off) → not 200
        responses.push_back((
            make_result(&[("e", InstructionStatus::Success)]),
            Some(vec![
                make_artifact("GladiatorsFinale", "flower", 20, true),
                make_artifact("WanderersTroupe", "plume", 16, false),
            ]),
        ));
        responses.push_back((
            make_result(&[("f", InstructionStatus::Success)]),
            None,
        ));

        let (port, shutdown, handle) = start_test_server(responses, 0);
        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}", port);

        // === Pair 1: aborted scan invalidates cache ===
        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["b"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 503);

        // === Pair 2: lock job with no snapshot invalidates cache ===
        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["c"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["d"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 503);

        // === Pair 3: update_inventory off after on ===
        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["e"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body.as_array().unwrap().len(), 2);

        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["f"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_ne!(resp.status().as_u16(), 200,
            "/artifacts must not serve stale data after a scan with update_inventory OFF");

        stop_server(&shutdown, handle);
    }

    /// Manager disabled returns 503.
    #[test]
    fn test_manage_disabled_returns_503() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

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

    /// Busy-state behavior + mid-execution cache invalidation.
    /// Consolidates: test_busy_state_behavior, test_artifacts_cleared_immediately_when_job_starts.
    #[test]
    fn test_busy_and_delayed_jobs() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let mut responses = VecDeque::new();
        // Job 1: busy-state test (3s delay is enough — we check at 500ms)
        responses.push_back((
            make_result(&[("a", InstructionStatus::Success)]),
            None,
        ));
        // Job 2: populate snapshot for cache-clear test
        responses.push_back((
            make_result(&[("c", InstructionStatus::Success)]),
            Some(vec![make_artifact("GladiatorsFinale", "flower", 20, true)]),
        ));
        // Job 3: slow job, check cache cleared mid-execution
        responses.push_back((
            make_result(&[("d", InstructionStatus::Success)]),
            None,
        ));

        let (port, shutdown, handle) = start_test_server(responses, 3000);
        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}", port);

        // === Busy-state checks ===

        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 202);
        let body: serde_json::Value = resp.json().unwrap();
        let job_id = body["jobId"].as_str().unwrap().to_string();

        // Wait for job to start processing (past the 1s pre-delay)
        std::thread::sleep(Duration::from_millis(1500));

        // 409 when busy: second job rejected
        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["b"]))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 409);

        // health shows busy during job
        let resp = client.get(format!("{}/health", base)).send().unwrap();
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["busy"], true);

        // result returns 409 when still running
        let resp = client
            .get(format!("{}/result?jobId={}", base, job_id))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 409);

        poll_until_completed(port);

        // === Cache cleared mid-execution ===

        // Populate cache
        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["c"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        // Submit slow job and check cache while running
        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["d"]))
            .send()
            .unwrap();

        // Wait past 1s pre-delay for execution to start
        std::thread::sleep(Duration::from_millis(1500));

        // Cache must already be invalidated mid-execution
        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_ne!(resp.status().as_u16(), 200,
            "/artifacts must be cleared as soon as a lock job starts, not after it finishes");

        poll_until_completed(port);
        stop_server(&shutdown, handle);
    }

    /// Game init failure produces ui_error results for all items.
    #[test]
    fn test_game_init_failure_produces_ui_error_results() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

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
        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["x", "y"]))
            .send()
            .unwrap();
        let submit_body: serde_json::Value = resp.json().unwrap();
        let job_id = submit_body["jobId"].as_str().unwrap().to_string();
        poll_until_completed(port);

        // Check result
        let resp = client.get(format!("{}/result?jobId={}", base, job_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        let results = body["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["status"], "ui_error");
        assert_eq!(results[1]["status"], "ui_error");

        stop_server(&shutdown, handle);
    }
}
