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
use yas::{log_error, log_info, log_warn};
use tiny_http::{Header, Method, Response, Server};

use crate::cli::{GoodUserConfig, ScanCoreConfig};
use crate::manager::models::*;
use crate::manager::orchestrator::ArtifactManager;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::manager::orchestrator::ProgressFn;
use crate::scanner::common::models::{GoodArtifact, GoodCharacter, GoodWeapon};

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
    Scan(ScanRequest),
}

/// Result of a single scan phase.
///
/// A phase is `Complete` only if every instance in the category was scanned
/// in this run. Any abort (user RMB, cancel token, errors mid-scan) yields
/// `Incomplete` so no data is published.
pub enum PhaseResult<T> {
    /// Phase was not requested by the client.
    NotAttempted,
    /// Phase was requested but did not finish (aborted, errored, or never started because an earlier phase aborted).
    Incomplete,
    /// Phase finished successfully with full data.
    Complete(Vec<T>),
}

/// Result of a scan execution. Each category reports Complete/Incomplete/NotAttempted.
pub struct ScanResult {
    pub characters: PhaseResult<GoodCharacter>,
    pub weapons: PhaseResult<GoodWeapon>,
    pub artifacts: PhaseResult<GoodArtifact>,
}

/// Abstraction over game interaction for testability.
pub trait ManageExecutor {
    fn execute(
        &mut self,
        request: LockManageRequest,
        progress_fn: Option<&ProgressFn<'_>>,
        cancel_token: yas::cancel::CancelToken,
    ) -> (ManageResult, Option<Vec<GoodArtifact>>);

    fn execute_equip(
        &mut self,
        request: EquipRequest,
        progress_fn: Option<&ProgressFn<'_>>,
        cancel_token: yas::cancel::CancelToken,
    ) -> ManageResult;

    fn execute_scan(
        &mut self,
        request: &ScanRequest,
        progress_fn: Option<&crate::scanner::common::progress::ProgressFn<'_>>,
        cancel_token: yas::cancel::CancelToken,
    ) -> anyhow::Result<ScanResult>;
}

/// Real executor: wraps a game controller and artifact manager.
pub struct GameExecutor {
    pub ctrl: GenshinGameController,
    pub manager: ArtifactManager,
    pub user_config: GoodUserConfig,
    pub scan_defaults: ScanCoreConfig,
}

impl ManageExecutor for GameExecutor {
    fn execute(
        &mut self,
        request: LockManageRequest,
        progress_fn: Option<&ProgressFn<'_>>,
        cancel_token: yas::cancel::CancelToken,
    ) -> (ManageResult, Option<Vec<GoodArtifact>>) {
        self.manager.execute(&mut self.ctrl, request, progress_fn, cancel_token)
    }

    fn execute_equip(
        &mut self,
        request: EquipRequest,
        progress_fn: Option<&ProgressFn<'_>>,
        cancel_token: yas::cancel::CancelToken,
    ) -> ManageResult {
        self.manager.execute_equip(&mut self.ctrl, request, progress_fn, cancel_token)
    }

    fn execute_scan(
        &mut self,
        request: &ScanRequest,
        progress_fn: Option<&crate::scanner::common::progress::ProgressFn<'_>>,
        cancel_token: yas::cancel::CancelToken,
    ) -> anyhow::Result<ScanResult> {
        use crate::cli::GoodScannerApplication;
        use crate::scanner::artifact::GoodArtifactScanner;
        use crate::scanner::character::GoodCharacterScanner;
        use crate::scanner::weapon::GoodWeaponScanner;

        let scanner_config = self.scan_defaults.to_scanner_config();
        let mappings = self.manager.mappings().clone();
        let pools = self.manager.pools().clone();

        // Match manager/equip order: focus the game window first, then arm the cancel token.
        self.ctrl.focus_game_window();
        self.ctrl.set_cancel_token(cancel_token.clone());

        // Per-category wrapper: scanners emit progress_fn(c, t, id, "").
        // We rewrite the phase string to the category key so the server
        // dispatch routes each tick into the right PhaseProgress slot.
        let chars_progress = |c: usize, t: usize, id: &str, _phase: &str| {
            if let Some(outer) = progress_fn {
                outer(c, t, id, "characters");
            }
        };
        let weapons_progress = |c: usize, t: usize, id: &str, _phase: &str| {
            if let Some(outer) = progress_fn {
                outer(c, t, id, "weapons");
            }
        };
        let artifacts_progress = |c: usize, t: usize, id: &str, _phase: &str| {
            if let Some(outer) = progress_fn {
                outer(c, t, id, "artifacts");
            }
        };

        let mut characters: PhaseResult<GoodCharacter> = PhaseResult::NotAttempted;
        let mut weapons: PhaseResult<GoodWeapon> = PhaseResult::NotAttempted;
        let mut artifacts: PhaseResult<GoodArtifact> = PhaseResult::NotAttempted;

        if request.characters {
            characters = if cancel_token.is_cancelled() {
                PhaseResult::Incomplete
            } else {
                let cfg = GoodScannerApplication::make_char_config(&scanner_config, &self.user_config);
                let scan_result = match GoodCharacterScanner::new(cfg, mappings.clone()) {
                    Ok(scanner) => scanner.scan(
                        &mut self.ctrl, 0, &pools,
                        Some(&chars_progress),
                    ),
                    Err(e) => Err(e),
                };
                if let Err(ref e) = scan_result {
                    log_warn!("[scan] 角色阶段失败: {:#}", "[scan] character phase failed: {:#}", e);
                }
                let phase = match scan_result {
                    Ok(data) if !cancel_token.is_cancelled() => PhaseResult::Complete(data),
                    _ => PhaseResult::Incomplete,
                };
                if matches!(phase, PhaseResult::Complete(_)) && !cancel_token.is_cancelled() {
                    self.ctrl.return_to_main_ui(4);
                }
                phase
            };
        }

        if request.weapons {
            weapons = if cancel_token.is_cancelled() {
                PhaseResult::Incomplete
            } else {
                let cfg = GoodScannerApplication::make_weapon_config(&scanner_config, &self.user_config);
                let scan_result = match GoodWeaponScanner::new(cfg, mappings.clone()) {
                    Ok(scanner) => scanner.scan(
                        &mut self.ctrl, false, 0, &pools,
                        Some(&weapons_progress),
                    ),
                    Err(e) => Err(e),
                };
                if let Err(ref e) = scan_result {
                    log_warn!("[scan] 武器阶段失败: {:#}", "[scan] weapon phase failed: {:#}", e);
                }
                match scan_result {
                    Ok(data) if !cancel_token.is_cancelled() => PhaseResult::Complete(data),
                    _ => PhaseResult::Incomplete,
                }
            };
        }

        if request.artifacts {
            artifacts = if cancel_token.is_cancelled() {
                PhaseResult::Incomplete
            } else {
                let cfg = GoodScannerApplication::make_artifact_config(&scanner_config, &self.user_config);
                // Only skip the open-backpack step if the weapons phase fully completed
                // (i.e. the backpack is reliably open). Otherwise reopen from scratch.
                let skip_open = matches!(weapons, PhaseResult::Complete(_));
                let scan_result = match GoodArtifactScanner::new(cfg, mappings.clone()) {
                    Ok(scanner) => scanner.scan(
                        &mut self.ctrl, skip_open, 0, &pools,
                        Some(&artifacts_progress),
                    ),
                    Err(e) => Err(e),
                };
                if let Err(ref e) = scan_result {
                    log_warn!("[scan] 圣遗物阶段失败: {:#}", "[scan] artifact phase failed: {:#}", e);
                }
                match scan_result {
                    Ok(data) if !cancel_token.is_cancelled() => PhaseResult::Complete(data),
                    _ => PhaseResult::Incomplete,
                }
            };
        }

        Ok(ScanResult { characters, weapons, artifacts })
    }
}

/// Maximum request body size (5 MB).
const MAX_BODY_SIZE: usize = 5 * 1024 * 1024;

/// Generic scan data cache: stores the latest results for a given data type
/// along with the jobId that produced them.
///
/// `incomplete_job_id` records the most recent job that attempted to populate
/// this cache but failed to complete (aborted by user, errored, or stopped
/// before reaching this category). Queries matching that id return 503 so the
/// client can distinguish "scan was aborted" from "no such job".
struct ScanDataCache<T> {
    job_id: Option<String>,
    data: Option<Vec<T>>,
    incomplete_job_id: Option<String>,
}

impl<T> ScanDataCache<T> {
    fn empty() -> Self {
        Self { job_id: None, data: None, incomplete_job_id: None }
    }

    fn set(&mut self, job_id: String, data: Vec<T>) {
        self.job_id = Some(job_id);
        self.data = Some(data);
        self.incomplete_job_id = None;
    }

    fn mark_incomplete(&mut self, job_id: String) {
        self.incomplete_job_id = Some(job_id);
    }

    fn invalidate(&mut self) {
        self.data = None;
        self.job_id = None;
        self.incomplete_job_id = None;
    }
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
    F: FnMut() -> anyhow::Result<Box<dyn ManageExecutor>>,
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

    // Per-type data caches (populated by scan/manage jobs).
    let character_cache: Arc<Mutex<ScanDataCache<GoodCharacter>>> =
        Arc::new(Mutex::new(ScanDataCache::empty()));
    let weapon_cache: Arc<Mutex<ScanDataCache<GoodWeapon>>> =
        Arc::new(Mutex::new(ScanDataCache::empty()));
    let artifact_cache: Arc<Mutex<ScanDataCache<GoodArtifact>>> =
        Arc::new(Mutex::new(ScanDataCache::empty()));

    // Channel for submitting jobs from HTTP thread to execution thread
    let (job_tx, job_rx) = mpsc::channel::<(String, JobRequest)>();

    // Clone shared refs for the HTTP thread
    let http_state = job_state.clone();
    let http_enabled = enabled.clone();
    let http_character_cache = character_cache.clone();
    let http_weapon_cache = weapon_cache.clone();
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
                    log_warn!("CORS preflight 响应失败: {}", "CORS preflight response failed: {}", e);
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

                (Method::Post, "/scan") => {
                    handle_scan(request, &http_enabled, &http_state, &http_job_tx, cors_ref);
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

                // GET /characters?jobId=xxx
                (Method::Get, url) if url.starts_with("/characters") => {
                    serve_cache(request, url, &http_character_cache, "characters", cors_ref);
                }

                // GET /weapons?jobId=xxx
                (Method::Get, url) if url.starts_with("/weapons") => {
                    serve_cache(request, url, &http_weapon_cache, "weapons", cors_ref);
                }

                // GET /artifacts?jobId=xxx (jobId optional for backwards compat)
                (Method::Get, url) if url.starts_with("/artifacts") => {
                    serve_artifact_cache(request, url, &http_artifact_cache, cors_ref);
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
    let mut executor: Option<Box<dyn ManageExecutor>> = None;
    let mut init_executor = init_executor;

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

        // Lazy init: create executor if we don't have one yet. On failure we
        // do NOT poison the server — the next job gets a fresh attempt, since
        // init_executor is FnMut and the user may have just needed to open
        // the game window.
        if executor.is_none() {
            match init_executor() {
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
                        JobRequest::Scan(r) => r.characters as usize + r.weapons as usize + r.artifacts as usize,
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

        let exec = executor.as_mut().unwrap();

        // Immediately invalidate cached data before execution starts.
        // Lock/unlock/equip changes modify in-game state; clients must not read stale data.
        {
            let invalidate_now = match &request {
                JobRequest::Manage(r) => !r.lock.is_empty() || !r.unlock.is_empty(),
                JobRequest::Equip(_) => true,
                JobRequest::Scan(_) => false, // scan is read-only
            };
            if invalidate_now {
                let mut cache = artifact_cache.lock().unwrap();
                if cache.data.is_some() {
                    cache.invalidate();
                }
            }
        }

        // Linear progress_fn for manage/equip: writes into JobState.progress.
        let linear_state = job_state.clone();
        let linear_progress_fn = move |completed: usize, total: usize, current_id: &str, phase: &str| {
            if let Ok(mut state) = linear_state.lock() {
                state.progress = Some(JobProgress {
                    completed,
                    total,
                    current_id: current_id.to_string(),
                    phase: phase.to_string(),
                });
            }
        };

        // Scan progress_fn: `phase` is the category key ("characters" /
        // "weapons" / "artifacts"). Updates the per-category slot in
        // JobState.scan_progress. Transitions phase state to Running on the
        // first tick; Complete/Aborted are set when execute_scan returns.
        let scan_state = job_state.clone();
        let scan_progress_fn = move |completed: usize, total: usize, _id: &str, phase: &str| {
            if let Ok(mut state) = scan_state.lock() {
                if let Some(ref mut sp) = state.scan_progress {
                    let slot = match phase {
                        "characters" => sp.characters.as_mut(),
                        "weapons" => sp.weapons.as_mut(),
                        "artifacts" => sp.artifacts.as_mut(),
                        _ => None,
                    };
                    if let Some(pp) = slot {
                        pp.completed = completed;
                        pp.total = total;
                        pp.state = PhaseState::Running;
                    }
                }
            }
        };

        let cancel_token = yas::cancel::CancelToken::new();

        // Dispatch: manage/equip use ManageResult; scan builds its own ManageResult summary.
        enum JobOutcome {
            ManageEquip {
                result: ManageResult,
                artifact_snapshot: Option<Vec<GoodArtifact>>,
                invalidates_cache: bool,
            },
            Scan(anyhow::Result<ScanResult>),
        }

        let outcome = match std::panic::catch_unwind(
            std::panic::AssertUnwindSafe(|| match request {
                JobRequest::Manage(manage_req) => {
                    let has_lock = !manage_req.lock.is_empty() || !manage_req.unlock.is_empty();
                    let (result, snapshot) = exec.execute(manage_req, Some(&linear_progress_fn), cancel_token);
                    JobOutcome::ManageEquip { result, artifact_snapshot: snapshot, invalidates_cache: has_lock }
                }
                JobRequest::Equip(equip_req) => {
                    let result = exec.execute_equip(equip_req, Some(&linear_progress_fn), cancel_token);
                    JobOutcome::ManageEquip { result, artifact_snapshot: None, invalidates_cache: true }
                }
                JobRequest::Scan(scan_req) => {
                    JobOutcome::Scan(exec.execute_scan(&scan_req, Some(&scan_progress_fn), cancel_token))
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

        match outcome {
            JobOutcome::ManageEquip { result, artifact_snapshot, invalidates_cache } => {
                // Update artifact cache based on scan completeness
                match artifact_snapshot {
                    Some(snapshot) => {
                        let count = snapshot.len();
                        artifact_cache.lock().unwrap().set(job_id.clone(), snapshot);
                        log_info!("[job {}] 圣遗物快照已更新（{} 个）", "[job {}] Artifact snapshot updated ({} items)", job_id, count);
                    }
                    None => {
                        if invalidates_cache {
                            let mut cache = artifact_cache.lock().unwrap();
                            if cache.data.is_some() {
                                cache.invalidate();
                                log_info!("[job {}] 游戏内状态已变更，快照已失效", "[job {}] In-game state changed, artifact snapshot invalidated", job_id);
                            }
                        }
                    }
                }
                let mut state = job_state.lock().unwrap();
                *state = JobState::completed(job_id.clone(), result);
            }
            JobOutcome::Scan(scan_result) => {
                match scan_result {
                    Ok(sr) => {
                        // Per-phase: Complete populates the cache; Incomplete marks the
                        // cache as incomplete-for-this-jobId (so /characters /weapons
                        // /artifacts queries with this jobId return 503); NotAttempted
                        // leaves the cache untouched.
                        let mut results = Vec::new();
                        let mut phases_complete = 0usize;
                        let mut phases_incomplete = 0usize;

                        match sr.characters {
                            PhaseResult::Complete(data) => {
                                character_cache.lock().unwrap().set(job_id.clone(), data);
                                phases_complete += 1;
                                results.push(InstructionResult {
                                    id: "characters".to_string(),
                                    status: InstructionStatus::Success,
                                });
                            }
                            PhaseResult::Incomplete => {
                                character_cache.lock().unwrap().mark_incomplete(job_id.clone());
                                phases_incomplete += 1;
                                results.push(InstructionResult {
                                    id: "characters".to_string(),
                                    status: InstructionStatus::Aborted,
                                });
                            }
                            PhaseResult::NotAttempted => {}
                        }

                        match sr.weapons {
                            PhaseResult::Complete(data) => {
                                weapon_cache.lock().unwrap().set(job_id.clone(), data);
                                phases_complete += 1;
                                results.push(InstructionResult {
                                    id: "weapons".to_string(),
                                    status: InstructionStatus::Success,
                                });
                            }
                            PhaseResult::Incomplete => {
                                weapon_cache.lock().unwrap().mark_incomplete(job_id.clone());
                                phases_incomplete += 1;
                                results.push(InstructionResult {
                                    id: "weapons".to_string(),
                                    status: InstructionStatus::Aborted,
                                });
                            }
                            PhaseResult::NotAttempted => {}
                        }

                        match sr.artifacts {
                            PhaseResult::Complete(data) => {
                                artifact_cache.lock().unwrap().set(job_id.clone(), data);
                                phases_complete += 1;
                                results.push(InstructionResult {
                                    id: "artifacts".to_string(),
                                    status: InstructionStatus::Success,
                                });
                            }
                            PhaseResult::Incomplete => {
                                artifact_cache.lock().unwrap().mark_incomplete(job_id.clone());
                                phases_incomplete += 1;
                                results.push(InstructionResult {
                                    id: "artifacts".to_string(),
                                    status: InstructionStatus::Aborted,
                                });
                            }
                            PhaseResult::NotAttempted => {}
                        }

                        log_info!(
                            "[job {}] 扫描结束（{} 完成, {} 中断）",
                            "[job {}] Scan finished ({} complete, {} aborted)",
                            job_id, phases_complete, phases_incomplete
                        );
                        let summary = ManageSummary::from_results(&results);
                        let result = ManageResult { results, summary };
                        let mut state = job_state.lock().unwrap();
                        *state = JobState::completed(job_id.clone(), result);
                    }
                    Err(e) => {
                        log_error!("[job {}] 扫描失败: {:#}", "[job {}] Scan failed: {:#}", job_id, e);
                        let summary = ManageSummary {
                            total: 0, success: 0, already_correct: 0, not_found: 0,
                            errors: 1, aborted: 0,
                        };
                        let result = ManageResult { results: Vec::new(), summary };
                        let mut state = job_state.lock().unwrap();
                        *state = JobState::completed(job_id.clone(), result);
                    }
                }
            }
        }

        log_info!("[job {}] 执行完成", "[job {}] Execution completed", job_id);
    }

    // Channel disconnected — wait for internal threads to fully stop before
    // returning. Without this, detached threads may still be tearing down
    // when the process exits, causing heap corruption in test suites.
    let _ = shutdown_watcher.join();
    let _ = http_thread.join();
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

/// Parse jobId from a URL query string like "/path?jobId=xxx".
fn parse_job_id(url: &str) -> Option<&str> {
    url.split('?')
        .nth(1)
        .and_then(|qs| qs.split('&').find(|p| p.starts_with("jobId=")))
        .map(|p| &p[6..])
        .filter(|s| !s.is_empty())
}

/// Serve a typed data cache endpoint (GET /characters, /weapons, /artifacts).
/// Requires `?jobId=xxx` query parameter.
///
/// 200: cached data for matching jobId.
/// 503: the requested jobId attempted to populate this cache but didn't finish.
/// 404: unknown jobId (never seen, or overwritten by a later scan).
/// 400: jobId query parameter missing.
fn serve_cache<T: serde::Serialize>(
    request: tiny_http::Request,
    url: &str,
    cache: &Arc<Mutex<ScanDataCache<T>>>,
    label: &str,
    cors_origin: Option<&str>,
) {
    let query_job_id = parse_job_id(url);
    match query_job_id {
        None => {
            respond_json(request, 400,
                r#"{"error":"missing required query parameter: jobId"}"#, cors_origin);
        }
        Some(requested_id) => {
            let c = cache.lock().unwrap();
            if let (Some(cached_id), Some(data)) = (&c.job_id, &c.data) {
                if cached_id == requested_id {
                    let json = serde_json::to_string(data).unwrap_or_else(|_| {
                        r#"{"error":"serialization failed"}"#.to_string()
                    });
                    drop(c);
                    respond_json(request, 200, &json, cors_origin);
                    return;
                }
            }
            if c.incomplete_job_id.as_deref() == Some(requested_id) {
                drop(c);
                respond_json(request, 503,
                    &format!(r#"{{"error":"{} scan incomplete for this jobId"}}"#, label),
                    cors_origin);
                return;
            }
            drop(c);
            respond_json(request, 404,
                &format!(r#"{{"error":"no {} data for this jobId"}}"#, label),
                cors_origin);
        }
    }
}

/// Serve the artifact cache with optional jobId (backwards compatible).
/// If jobId is provided, it must match. If omitted, returns the latest data.
///
/// 200: cached data matching jobId (or latest, if jobId omitted).
/// 503: the requested jobId attempted to populate the artifact cache but didn't finish.
/// 404: no cached data, or jobId specified but not recognized.
fn serve_artifact_cache(
    request: tiny_http::Request,
    url: &str,
    cache: &Arc<Mutex<ScanDataCache<GoodArtifact>>>,
    cors_origin: Option<&str>,
) {
    let query_job_id = parse_job_id(url);
    let c = cache.lock().unwrap();
    if let (Some(cached_id), Some(data)) = (&c.job_id, &c.data) {
        // If jobId provided, it must match; otherwise serve the latest.
        if query_job_id.map_or(true, |q| q == cached_id) {
            let json = serde_json::to_string(data).unwrap_or_else(|_| {
                r#"{"error":"serialization failed"}"#.to_string()
            });
            drop(c);
            respond_json(request, 200, &json, cors_origin);
            return;
        }
    }
    if let Some(requested_id) = query_job_id {
        if c.incomplete_job_id.as_deref() == Some(requested_id) {
            drop(c);
            respond_json(request, 503,
                r#"{"error":"artifacts scan incomplete for this jobId"}"#,
                cors_origin);
            return;
        }
    }
    drop(c);
    respond_json(request, 404,
        &format!(r#"{{"error":"{}"}}"#, yas::lang::localize(
            "没有可用的圣遗物数据 / No artifact data available"
        )),
        cors_origin);
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

/// Handle POST /scan: validate, parse ScanRequest, submit job.
fn handle_scan(
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

    save_request("scan", &body);

    let scan_request: ScanRequest = match serde_json::from_str(&body) {
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

    if !scan_request.characters && !scan_request.weapons && !scan_request.artifacts {
        respond_json(
            request,
            400,
            &format!(r#"{{"error":"{}"}}"#, yas::lang::localize("至少需要一个扫描目标 / At least one scan target must be true")),
            cors_origin,
        );
        return;
    }

    let scan_chars = scan_request.characters;
    let scan_wpns = scan_request.weapons;
    let scan_arts = scan_request.artifacts;
    let job_id = uuid::Uuid::new_v4().to_string();

    log_info!(
        "[job {}] 收到扫描请求（角色: {}, 武器: {}, 圣遗物: {}）",
        "[job {}] Received scan request (characters: {}, weapons: {}, artifacts: {})",
        job_id, scan_chars, scan_wpns, scan_arts
    );

    {
        let mut s = state.lock().unwrap();
        *s = JobState::running_scan(job_id.clone(), scan_chars, scan_wpns, scan_arts);
    }

    if job_tx.send((job_id.clone(), JobRequest::Scan(scan_request))).is_err() {
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

    let json = format!(
        r#"{{"jobId":"{}","targets":{{"characters":{},"weapons":{},"artifacts":{}}}}}"#,
        job_id, scan_chars, scan_wpns, scan_arts
    );
    respond_json(request, 202, &json, cors_origin);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::common::models::{GoodArtifact, GoodCharacter, GoodTalent, GoodWeapon, GoodSubStat};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    // Serialize all server tests to prevent concurrent tiny_http teardown,
    // which causes STATUS_HEAP_CORRUPTION on Windows.
    static SERVER_LOCK: Mutex<()> = Mutex::new(());

    struct FakeExecutor {
        responses: Arc<Mutex<VecDeque<(ManageResult, Option<Vec<GoodArtifact>>)>>>,
        scan_responses: Arc<Mutex<VecDeque<anyhow::Result<ScanResult>>>>,
        delay_ms: u64,
    }

    impl ManageExecutor for FakeExecutor {
        fn execute(
            &mut self,
            _request: LockManageRequest,
            progress_fn: Option<&ProgressFn<'_>>,
            _cancel_token: yas::cancel::CancelToken,
        ) -> (ManageResult, Option<Vec<GoodArtifact>>) {
            let (result, snapshot) = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("FakeExecutor: no more responses queued");

            // Report per-item progress spread across delay_ms so polling clients
            // can observe intermediate values. Mirrors real orchestrator behaviour
            // of reporting `completed` ticks from 0 to N.
            let total = result.results.len();
            if let Some(pf) = progress_fn {
                pf(0, total, "", "锁定变更 / Lock changes");
            }
            let per_item_delay = if total > 0 { self.delay_ms / total as u64 } else { self.delay_ms };
            for (idx, r) in result.results.iter().enumerate() {
                if per_item_delay > 0 {
                    std::thread::sleep(Duration::from_millis(per_item_delay));
                }
                if let Some(pf) = progress_fn {
                    pf(idx + 1, total, &r.id, "锁定变更 / Lock changes");
                }
            }
            (result, snapshot)
        }

        fn execute_equip(
            &mut self,
            _request: EquipRequest,
            _progress_fn: Option<&ProgressFn<'_>>,
            _cancel_token: yas::cancel::CancelToken,
        ) -> ManageResult {
            let results = Vec::new();
            let summary = ManageSummary::from_results(&results);
            ManageResult { results, summary }
        }

        fn execute_scan(
            &mut self,
            _request: &ScanRequest,
            progress_fn: Option<&crate::scanner::common::progress::ProgressFn<'_>>,
            _cancel_token: yas::cancel::CancelToken,
        ) -> anyhow::Result<ScanResult> {
            let outcome = self
                .scan_responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("FakeExecutor: no more scan responses queued");

            // Emit per-category per-item ticks spread across delay_ms. Each
            // requested phase gets a fake "total = 10" and ticks through 10 items
            // so polling clients can observe intermediate (completed, total)
            // values inside each category bar.
            let phases: Vec<(&'static str, bool)> = match &outcome {
                Ok(sr) => vec![
                    ("characters", !matches!(sr.characters, PhaseResult::NotAttempted)),
                    ("weapons", !matches!(sr.weapons, PhaseResult::NotAttempted)),
                    ("artifacts", !matches!(sr.artifacts, PhaseResult::NotAttempted)),
                ],
                Err(_) => vec![],
            };
            let active_phases: Vec<&'static str> = phases.iter()
                .filter_map(|(k, active)| if *active { Some(*k) } else { None })
                .collect();
            let fake_total: usize = 10;
            let total_ticks = active_phases.len() * fake_total;
            let per_tick_delay = if total_ticks > 0 { self.delay_ms / total_ticks as u64 } else { 0 };
            for phase_key in &active_phases {
                if let Some(pf) = progress_fn {
                    pf(0, fake_total, "", phase_key);
                }
                for i in 0..fake_total {
                    if per_tick_delay > 0 {
                        std::thread::sleep(Duration::from_millis(per_tick_delay));
                    }
                    if let Some(pf) = progress_fn {
                        pf(i + 1, fake_total, "", phase_key);
                    }
                }
            }
            outcome
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
                rolls: vec![],
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
        start_test_server_full(responses, VecDeque::new(), delay_ms, Arc::new(AtomicBool::new(true)))
    }

    fn start_test_server_with_enabled(
        responses: VecDeque<(ManageResult, Option<Vec<GoodArtifact>>)>,
        delay_ms: u64,
        enabled: Arc<AtomicBool>,
    ) -> (u16, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        start_test_server_full(responses, VecDeque::new(), delay_ms, enabled)
    }

    fn start_test_server_with_scans(
        responses: VecDeque<(ManageResult, Option<Vec<GoodArtifact>>)>,
        scan_responses: VecDeque<anyhow::Result<ScanResult>>,
        delay_ms: u64,
    ) -> (u16, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        start_test_server_full(responses, scan_responses, delay_ms, Arc::new(AtomicBool::new(true)))
    }

    fn start_test_server_full(
        responses: VecDeque<(ManageResult, Option<Vec<GoodArtifact>>)>,
        scan_responses: VecDeque<anyhow::Result<ScanResult>>,
        delay_ms: u64,
        enabled: Arc<AtomicBool>,
    ) -> (u16, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let port = next_port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let responses = Arc::new(Mutex::new(responses));
        let responses_clone = responses.clone();
        let scan_responses = Arc::new(Mutex::new(scan_responses));
        let scan_responses_clone = scan_responses.clone();

        let handle = std::thread::spawn(move || {
            let init = move || -> anyhow::Result<Box<dyn ManageExecutor>> {
                Ok(Box::new(FakeExecutor {
                    responses: responses_clone.clone(),
                    scan_responses: scan_responses_clone.clone(),
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

        // artifacts: 404 before any scan (no jobId required)
        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        // artifacts: 404 with unknown jobId
        let resp = client.get(format!("{}/artifacts?jobId=nonexistent", base)).send().unwrap();
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
        let job1_early_id = body["jobId"].as_str().unwrap().to_string();
        assert_eq!(body["total"], 1);

        poll_until_completed(port);

        // No snapshot → artifacts 404 for this jobId
        let resp = client.get(format!("{}/artifacts?jobId={}", base, job1_early_id)).send().unwrap();
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

        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["art1"]))
            .send()
            .unwrap();
        let job3_id = resp.json::<serde_json::Value>().unwrap()["jobId"]
            .as_str().unwrap().to_string();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts?jobId={}", base, job3_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert!(body.is_array());
        assert_eq!(body.as_array().unwrap().len(), 2);
        assert_eq!(body[0]["setKey"], "GladiatorsFinale");
        assert_eq!(body[1]["setKey"], "WanderersTroupe");

        // /artifacts without jobId returns latest (backwards compat)
        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body.as_array().unwrap().len(), 2);

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
        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send()
            .unwrap();
        let job_a = resp.json::<serde_json::Value>().unwrap()["jobId"]
            .as_str().unwrap().to_string();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts?jobId={}", base, job_a)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["b"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        // Cache invalidated — old jobId no longer works
        let resp = client.get(format!("{}/artifacts?jobId={}", base, job_a)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);
        // Also 404 without jobId (no data at all after invalidation)
        let resp = client.get(format!("{}/artifacts", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        // === Pair 2: lock job with no snapshot invalidates cache ===
        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["c"]))
            .send()
            .unwrap();
        let job_c = resp.json::<serde_json::Value>().unwrap()["jobId"]
            .as_str().unwrap().to_string();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts?jobId={}", base, job_c)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["d"]))
            .send()
            .unwrap();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts?jobId={}", base, job_c)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        // === Pair 3: update_inventory off after on ===
        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["e"]))
            .send()
            .unwrap();
        let job_e = resp.json::<serde_json::Value>().unwrap()["jobId"]
            .as_str().unwrap().to_string();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts?jobId={}", base, job_e)).send().unwrap();
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

        let resp = client.get(format!("{}/artifacts?jobId={}", base, job_e)).send().unwrap();
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
        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["c"]))
            .send()
            .unwrap();
        let job_c = resp.json::<serde_json::Value>().unwrap()["jobId"]
            .as_str().unwrap().to_string();
        poll_until_completed(port);

        let resp = client.get(format!("{}/artifacts?jobId={}", base, job_c)).send().unwrap();
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
        let resp = client.get(format!("{}/artifacts?jobId={}", base, job_c)).send().unwrap();
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

    fn make_character(key: &str, level: i32) -> GoodCharacter {
        GoodCharacter {
            key: key.to_string(),
            level,
            constellation: 0,
            ascension: 6,
            talent: GoodTalent { auto: 1, skill: 1, burst: 1 },
            element: None,
        }
    }

    fn make_weapon(key: &str, level: i32) -> GoodWeapon {
        GoodWeapon {
            key: key.to_string(),
            level,
            ascension: 6,
            refinement: 1,
            rarity: 5,
            location: String::new(),
            lock: false,
        }
    }

    /// Scan API: full E2E flow — submit, poll, fetch results from each data endpoint.
    /// Also tests: validation (empty targets), jobId mismatch, scan after manage updates artifact cache.
    #[test]
    fn test_scan_api_flow() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let manage_responses = VecDeque::new();
        let mut scan_responses: VecDeque<anyhow::Result<ScanResult>> = VecDeque::new();

        // Scan 1: all three targets complete
        scan_responses.push_back(Ok(ScanResult {
            characters: PhaseResult::Complete(vec![
                make_character("Furina", 90),
                make_character("RaidenShogun", 80),
            ]),
            weapons: PhaseResult::Complete(vec![
                make_weapon("SkywardHarp", 90),
            ]),
            artifacts: PhaseResult::Complete(vec![
                make_artifact("GladiatorsFinale", "flower", 20, true),
            ]),
        }));

        // Scan 2: characters only
        scan_responses.push_back(Ok(ScanResult {
            characters: PhaseResult::Complete(vec![
                make_character("Nahida", 90),
            ]),
            weapons: PhaseResult::NotAttempted,
            artifacts: PhaseResult::NotAttempted,
        }));

        // Scan 3: scan error
        scan_responses.push_back(Err(anyhow::anyhow!("Game window not found")));

        // Scan 4: characters complete, weapons aborted, artifacts never reached
        scan_responses.push_back(Ok(ScanResult {
            characters: PhaseResult::Complete(vec![
                make_character("Furina", 90),
            ]),
            weapons: PhaseResult::Incomplete,
            artifacts: PhaseResult::Incomplete,
        }));

        let (port, shutdown, handle) = start_test_server_with_scans(
            manage_responses, scan_responses, 0,
        );
        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}", port);

        // === Validation: empty targets returns 400 ===

        let resp = client
            .post(format!("{}/scan", base))
            .header("Content-Type", "application/json")
            .body(r#"{"characters":false,"weapons":false,"artifacts":false}"#)
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400);

        // all-false via defaults (empty object)
        let resp = client
            .post(format!("{}/scan", base))
            .header("Content-Type", "application/json")
            .body(r#"{}"#)
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400);

        // bad JSON
        let resp = client
            .post(format!("{}/scan", base))
            .header("Content-Type", "application/json")
            .body("not json")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400);

        // === Data endpoints: 400 without jobId ===

        let resp = client.get(format!("{}/characters", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 400);
        let resp = client.get(format!("{}/weapons", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 400);

        // === Scan 1: all targets ===

        let resp = client
            .post(format!("{}/scan", base))
            .header("Content-Type", "application/json")
            .body(r#"{"characters":true,"weapons":true,"artifacts":true}"#)
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 202);
        let body: serde_json::Value = resp.json().unwrap();
        let scan1_id = body["jobId"].as_str().unwrap().to_string();
        assert_eq!(body["targets"]["characters"], true);
        assert_eq!(body["targets"]["weapons"], true);
        assert_eq!(body["targets"]["artifacts"], true);

        poll_until_completed(port);

        // /status shows completed with 3 phases
        let resp = client.get(format!("{}/status", base)).send().unwrap();
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["state"], "completed");
        assert_eq!(body["summary"]["total"], 3);
        assert_eq!(body["summary"]["success"], 3);

        // /result returns per-phase results
        let resp = client.get(format!("{}/result?jobId={}", base, scan1_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        let results = body["results"].as_array().unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0]["id"], "characters");
        assert_eq!(results[1]["id"], "weapons");
        assert_eq!(results[2]["id"], "artifacts");

        // /characters returns character data
        let resp = client.get(format!("{}/characters?jobId={}", base, scan1_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body.as_array().unwrap().len(), 2);
        assert_eq!(body[0]["key"], "Furina");
        assert_eq!(body[1]["key"], "RaidenShogun");

        // /weapons returns weapon data
        let resp = client.get(format!("{}/weapons?jobId={}", base, scan1_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["key"], "SkywardHarp");

        // /artifacts returns artifact data
        let resp = client.get(format!("{}/artifacts?jobId={}", base, scan1_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["setKey"], "GladiatorsFinale");

        // wrong jobId → 404
        let resp = client.get(format!("{}/characters?jobId=wrong", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);
        let resp = client.get(format!("{}/weapons?jobId=wrong", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);
        let resp = client.get(format!("{}/artifacts?jobId=wrong", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        // === Scan 2: characters only — weapons/artifacts keep scan1 data ===

        let resp = client
            .post(format!("{}/scan", base))
            .header("Content-Type", "application/json")
            .body(r#"{"characters":true}"#)
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 202);
        let body: serde_json::Value = resp.json().unwrap();
        let scan2_id = body["jobId"].as_str().unwrap().to_string();
        assert_eq!(body["targets"]["characters"], true);
        assert_eq!(body["targets"]["weapons"], false);
        assert_eq!(body["targets"]["artifacts"], false);

        poll_until_completed(port);

        // /result shows 1 phase
        let resp = client.get(format!("{}/result?jobId={}", base, scan2_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["results"].as_array().unwrap().len(), 1);
        assert_eq!(body["results"][0]["id"], "characters");

        // /characters with scan2 jobId returns new data
        let resp = client.get(format!("{}/characters?jobId={}", base, scan2_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["key"], "Nahida");

        // /characters with scan1 jobId → 404 (overwritten)
        let resp = client.get(format!("{}/characters?jobId={}", base, scan1_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        // /weapons still has scan1 data (scan2 didn't scan weapons)
        let resp = client.get(format!("{}/weapons?jobId={}", base, scan1_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body[0]["key"], "SkywardHarp");

        // /artifacts still has scan1 data
        let resp = client.get(format!("{}/artifacts?jobId={}", base, scan1_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        // === Scan 3: error — caches not updated ===

        let resp = client
            .post(format!("{}/scan", base))
            .header("Content-Type", "application/json")
            .body(r#"{"characters":true,"weapons":true,"artifacts":true}"#)
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 202);
        let body: serde_json::Value = resp.json().unwrap();
        let scan3_id = body["jobId"].as_str().unwrap().to_string();

        poll_until_completed(port);

        // /result shows error
        let resp = client.get(format!("{}/result?jobId={}", base, scan3_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["summary"]["errors"], 1);

        // Previous scan data still intact (error didn't wipe caches)
        let resp = client.get(format!("{}/characters?jobId={}", base, scan2_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let resp = client.get(format!("{}/weapons?jobId={}", base, scan1_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        // === Scan 4: characters complete, weapons + artifacts aborted ===

        let resp = client
            .post(format!("{}/scan", base))
            .header("Content-Type", "application/json")
            .body(r#"{"characters":true,"weapons":true,"artifacts":true}"#)
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 202);
        let body: serde_json::Value = resp.json().unwrap();
        let scan4_id = body["jobId"].as_str().unwrap().to_string();

        poll_until_completed(port);

        // /result: 1 success (characters) + 2 aborted (weapons, artifacts)
        let resp = client.get(format!("{}/result?jobId={}", base, scan4_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["summary"]["success"], 1);
        assert_eq!(body["summary"]["aborted"], 2);

        // Completed phase: /characters returns 200 for scan4
        let resp = client.get(format!("{}/characters?jobId={}", base, scan4_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body[0]["key"], "Furina");

        // Aborted phases: /weapons and /artifacts return 503 for scan4
        let resp = client.get(format!("{}/weapons?jobId={}", base, scan4_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 503);
        let resp = client.get(format!("{}/artifacts?jobId={}", base, scan4_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 503);

        // Old jobIds are no longer the cached id for characters (overwritten by scan4);
        // nothing was written for weapons/artifacts in scan4, so scan1 data is still served.
        let resp = client.get(format!("{}/characters?jobId={}", base, scan2_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);
        let resp = client.get(format!("{}/weapons?jobId={}", base, scan1_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let resp = client.get(format!("{}/artifacts?jobId={}", base, scan1_id)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        // Unknown jobId still returns 404 (not 503).
        let resp = client.get(format!("{}/weapons?jobId=nonexistent", base)).send().unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        stop_server(&shutdown, handle);
    }

    /// While a manage job is running, GET /status must expose intermediate
    /// `completed` values (not just 0 and N). Guards the full plumbing:
    /// `LockManager::execute` → progress_fn → JobState.progress →
    /// status_json → client response.
    #[test]
    fn test_manage_progress_visible_mid_run() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let mut responses = VecDeque::new();
        responses.push_back((
            make_result(&[
                ("a", InstructionStatus::Success),
                ("b", InstructionStatus::Success),
                ("c", InstructionStatus::Success),
                ("d", InstructionStatus::Success),
                ("e", InstructionStatus::Success),
            ]),
            None,
        ));

        // 1500ms total → 300ms between ticks. Gives the client plenty of time
        // to observe intermediate values with a tight poll loop.
        let (port, shutdown, handle) = start_test_server(responses, 1500);
        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}", port);

        let resp = client
            .post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a", "b", "c", "d", "e"]))
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 202);

        // Poll for up to ~4s (1s pre-delay + 1.5s execution + slack).
        let mut observed: Vec<(u64, u64)> = Vec::new();
        let mut total_observed: Option<u64> = None;
        for _ in 0..80 {
            std::thread::sleep(Duration::from_millis(50));
            let resp = client.get(format!("{}/status", base)).send().unwrap();
            let body: serde_json::Value = resp.json().unwrap();
            if body["state"] == "running" {
                if let (Some(c), Some(t)) = (
                    body["progress"]["completed"].as_u64(),
                    body["progress"]["total"].as_u64(),
                ) {
                    if observed.last() != Some(&(c, t)) {
                        observed.push((c, t));
                    }
                    total_observed = Some(t);
                }
            } else if body["state"] == "completed" {
                break;
            }
        }

        poll_until_completed(port);

        // Every running snapshot must have reported total=5.
        assert_eq!(total_observed, Some(5),
            "total field not reported through /status; observed: {:?}", observed);

        // Must observe at least one intermediate tick (completed > 0 && < total).
        // If we only see [0] and [5] the client has no per-item feedback.
        let has_intermediate = observed.iter().any(|&(c, t)| c > 0 && c < t);
        assert!(has_intermediate,
            "expected /status to expose intermediate completed values (per-item progress); observed: {:?}",
            observed);

        // Completed must monotonically increase.
        for w in observed.windows(2) {
            assert!(w[1].0 >= w[0].0,
                "completed regressed: {:?} -> {:?}", w[0], w[1]);
        }

        stop_server(&shutdown, handle);
    }

    /// Scan's per-category progress must flow through /status.scanProgress.
    /// Each of the 3 categories has its own (completed, total, state) and all
    /// three move independently — this is what lets the client draw 3 bars.
    #[test]
    fn test_scan_progress_visible_mid_run() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let mut scan_responses: VecDeque<anyhow::Result<ScanResult>> = VecDeque::new();
        scan_responses.push_back(Ok(ScanResult {
            characters: PhaseResult::Complete(vec![make_character("Furina", 90)]),
            weapons: PhaseResult::Complete(vec![make_weapon("SkywardHarp", 90)]),
            artifacts: PhaseResult::Complete(vec![
                make_artifact("GladiatorsFinale", "flower", 20, true),
            ]),
        }));

        // 1500ms across 30 ticks (3 phases × 10 items each) → 50ms per tick.
        let (port, shutdown, handle) = start_test_server_with_scans(
            VecDeque::new(), scan_responses, 1500,
        );
        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}", port);

        let resp = client
            .post(format!("{}/scan", base))
            .header("Content-Type", "application/json")
            .body(r#"{"characters":true,"weapons":true,"artifacts":true}"#)
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 202);

        #[derive(Clone, Debug)]
        struct CatObs {
            completed: u64,
            total: u64,
            state: String,
        }
        let mut chars_obs: Vec<CatObs> = Vec::new();
        let mut weapons_obs: Vec<CatObs> = Vec::new();
        let mut artifacts_obs: Vec<CatObs> = Vec::new();

        let record = |obs: &mut Vec<CatObs>, node: &serde_json::Value| {
            if let (Some(c), Some(t), Some(s)) = (
                node["completed"].as_u64(),
                node["total"].as_u64(),
                node["state"].as_str(),
            ) {
                let entry = CatObs { completed: c, total: t, state: s.to_string() };
                if obs.last().map(|p| (p.completed, p.total, p.state.as_str())
                    == (entry.completed, entry.total, entry.state.as_str())) != Some(true) {
                    obs.push(entry);
                }
            }
        };

        for _ in 0..120 {
            std::thread::sleep(Duration::from_millis(30));
            let resp = client.get(format!("{}/status", base)).send().unwrap();
            let body: serde_json::Value = resp.json().unwrap();
            if body["state"] == "running" {
                let sp = &body["scanProgress"];
                // progress.* should NOT be populated for scan jobs.
                assert!(body["progress"].is_null(),
                    "scan should use scanProgress, not the linear progress field; body={}",
                    body);
                if sp.is_object() {
                    record(&mut chars_obs, &sp["characters"]);
                    record(&mut weapons_obs, &sp["weapons"]);
                    record(&mut artifacts_obs, &sp["artifacts"]);
                }
            } else if body["state"] == "completed" {
                break;
            }
        }

        poll_until_completed(port);

        // All three categories must have been observed.
        assert!(!chars_obs.is_empty(), "no characters progress observed");
        assert!(!weapons_obs.is_empty(), "no weapons progress observed");
        assert!(!artifacts_obs.is_empty(), "no artifacts progress observed");

        // Each category must report intermediate completed values (not just 0 and total).
        let has_mid = |obs: &[CatObs]| obs.iter().any(|o| o.completed > 0 && o.completed < o.total);
        assert!(has_mid(&chars_obs),
            "expected intermediate characters progress; observed: {:?}", chars_obs);
        assert!(has_mid(&weapons_obs),
            "expected intermediate weapons progress; observed: {:?}", weapons_obs);
        assert!(has_mid(&artifacts_obs),
            "expected intermediate artifacts progress; observed: {:?}", artifacts_obs);

        // Each category transitioned from pending → running (FakeExecutor emits
        // running ticks; pending is only visible if we poll before that category
        // starts, which isn't guaranteed at these timing but the terminal state
        // we care about is Running).
        assert!(chars_obs.iter().any(|o| o.state == "running"));
        assert!(weapons_obs.iter().any(|o| o.state == "running"));
        assert!(artifacts_obs.iter().any(|o| o.state == "running"));

        // Completed must monotonically increase within each category.
        for obs in [&chars_obs, &weapons_obs, &artifacts_obs] {
            for w in obs.windows(2) {
                assert!(w[1].completed >= w[0].completed,
                    "completed regressed: {:?}", obs);
            }
        }

        stop_server(&shutdown, handle);
    }

    /// Init failure must not poison the server: the second job gets a fresh
    /// attempt. This guards against `init_executor.take()` semantics where a
    /// single failure would leave `executor = None` forever.
    #[test]
    fn test_init_failure_does_not_poison_server() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Custom init closure that fails the first two attempts, then succeeds.
        let responses: VecDeque<(ManageResult, Option<Vec<GoodArtifact>>)> = {
            let mut q = VecDeque::new();
            q.push_back((make_result(&[("a", InstructionStatus::Success)]), None));
            q
        };
        let responses = Arc::new(Mutex::new(responses));
        let scan_responses: Arc<Mutex<VecDeque<anyhow::Result<ScanResult>>>> =
            Arc::new(Mutex::new(VecDeque::new()));

        let port = next_port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let attempt_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempt_count_inner = attempt_count.clone();
        let responses_inner = responses.clone();
        let scan_responses_inner = scan_responses.clone();

        let handle = std::thread::spawn(move || {
            let init = move || -> anyhow::Result<Box<dyn ManageExecutor>> {
                let n = attempt_count_inner.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(anyhow::anyhow!("simulated init failure #{}", n))
                } else {
                    Ok(Box::new(FakeExecutor {
                        responses: responses_inner.clone(),
                        scan_responses: scan_responses_inner.clone(),
                        delay_ms: 0,
                    }))
                }
            };
            let _ = run_server(port, init, Arc::new(AtomicBool::new(true)), shutdown_clone);
        });

        // Wait for server to come up.
        let client = reqwest::blocking::Client::new();
        let health_url = format!("http://127.0.0.1:{}/health", port);
        for _ in 0..50 {
            if client.get(&health_url).send().is_ok() { break; }
            std::thread::sleep(Duration::from_millis(50));
        }
        let base = format!("http://127.0.0.1:{}", port);

        // Job 1: init fails. Summary shows 1 error, server stays usable.
        let resp = client.post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["x"]))
            .send().unwrap();
        assert_eq!(resp.status().as_u16(), 202);
        poll_until_completed(port);
        let body: serde_json::Value = client.get(format!("{}/status", base))
            .send().unwrap().json().unwrap();
        assert_eq!(body["summary"]["errors"], 1);

        // Job 2: init fails again — same 1 error summary, not a panic.
        let resp = client.post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["y"]))
            .send().unwrap();
        assert_eq!(resp.status().as_u16(), 202);
        poll_until_completed(port);
        let body: serde_json::Value = client.get(format!("{}/status", base))
            .send().unwrap().json().unwrap();
        assert_eq!(body["summary"]["errors"], 1);

        // Job 3: init succeeds — job runs and reports Success.
        let resp = client.post(format!("{}/manage", base))
            .header("Content-Type", "application/json")
            .body(make_manage_body(&["a"]))
            .send().unwrap();
        assert_eq!(resp.status().as_u16(), 202);
        poll_until_completed(port);
        let body: serde_json::Value = client.get(format!("{}/status", base))
            .send().unwrap().json().unwrap();
        assert_eq!(body["summary"]["success"], 1);

        // Init was attempted 3 times (two failures + one success).
        assert_eq!(attempt_count.load(Ordering::SeqCst), 3);

        stop_server(&shutdown, handle);
    }
}
