//! Async HTTP server for the artifact manager.
//!
//! Two-thread architecture:
//! - HTTP thread: handles all HTTP I/O (spawned)
//! - Execution thread: owns game controller, processes jobs (original thread)
//!
//! Communication: mpsc channel for job submission, Arc<Mutex<JobState>> for status.
//!
//! 异步 HTTP 服务器。双线程架构：HTTP 线程处理请求，执行线程控制游戏。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use anyhow::{anyhow, Result};
use log::{error, info, warn};
use tiny_http::{Header, Method, Response, Server};

use crate::manager::models::*;
use crate::manager::orchestrator::ArtifactManager;
use crate::scanner::common::game_controller::GenshinGameController;

/// CORS headers for browser access.
fn cors_headers() -> Vec<Header> {
    vec![
        Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap(),
        Header::from_bytes("Access-Control-Allow-Methods", "GET, POST, OPTIONS").unwrap(),
        Header::from_bytes("Access-Control-Allow-Headers", "Content-Type").unwrap(),
        Header::from_bytes("Content-Type", "application/json; charset=utf-8").unwrap(),
    ]
}

fn respond_json(request: tiny_http::Request, status: u16, json: &str) {
    let mut resp = Response::from_string(json).with_status_code(status);
    for header in cors_headers() {
        resp.add_header(header);
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

            // CORS preflight (always respond)
            if method == Method::Options {
                let mut resp = Response::empty(204);
                for header in cors_headers() {
                    resp.add_header(header);
                }
                let _ = request.respond(resp);
                continue;
            }

            match (method, url.as_str()) {
                (Method::Post, "/manage") => {
                    handle_manage(request, &http_enabled, &http_state, &job_tx);
                }

                (Method::Get, "/status") => {
                    let state = http_state.lock().unwrap();
                    let json = serde_json::to_string(&*state).unwrap_or_else(|_| {
                        r#"{"state":"idle"}"#.to_string()
                    });
                    drop(state);
                    respond_json(request, 200, &json);
                }

                (Method::Get, "/health") => {
                    let is_enabled = http_enabled.load(Ordering::Relaxed);
                    let state = http_state.lock().unwrap();
                    let is_busy = state.state == JobPhase::Running;
                    drop(state);
                    let json = format!(
                        r#"{{"status":"ok","enabled":{},"busy":{}}}"#,
                        is_enabled, is_busy
                    );
                    respond_json(request, 200, &json);
                }

                _ => {
                    respond_json(request, 404, r#"{"error":"Not Found"}"#);
                }
            }
        }
    });

    // Block on channel — zero CPU when idle, wakes instantly on job arrival.
    // This thread owns ctrl (which is !Send) so it must be the original thread.
    info!("执行线程就绪 / Execution thread ready");
    while let Ok((job_id, request)) = job_rx.recv() {
        info!("[job {}] 开始执行 / Starting execution", job_id);

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

/// Handle POST /manage: validate, check busy, submit job.
fn handle_manage(
    mut request: tiny_http::Request,
    enabled: &AtomicBool,
    state: &Arc<Mutex<JobState>>,
    job_tx: &mpsc::Sender<(String, ArtifactManageRequest)>,
) {
    // Check if manager is enabled
    if !enabled.load(Ordering::Relaxed) {
        warn!("管理器已暂停，拒绝请求 / Manager paused, rejecting request");
        respond_json(
            request,
            503,
            r#"{"error":"管理器已暂停 / Manager is paused. Enable it in the GUI to accept requests."}"#,
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
            );
            return;
        }
    };

    if manage_request.instructions.is_empty() {
        respond_json(
            request,
            400,
            r#"{"error":"指令列表为空 / Instructions list is empty"}"#,
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
        );
        return;
    }

    // Return 202 Accepted immediately
    let json = format!(r#"{{"jobId":"{}","total":{}}}"#, job_id, total);
    respond_json(request, 202, &json);
}
