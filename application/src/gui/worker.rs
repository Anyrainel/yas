use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use super::state::{AppState, Lang, TaskStatus};
use yas_genshin::cli::GoodUserConfig;

/// Handle to a running background task.
pub struct TaskHandle {
    _handle: JoinHandle<()>,
    /// Optional shutdown flag — set to true to request graceful stop.
    shutdown: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Optional cancel token for scan operations.
    cancel_token: Option<yas::cancel::CancelToken>,
}

impl TaskHandle {
    pub fn is_finished(&self) -> bool {
        self._handle.is_finished()
    }

    /// Signal the task to shut down gracefully.
    pub fn stop(&self) {
        if let Some(ref flag) = self.shutdown {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(ref token) = self.cancel_token {
            token.cancel(yas::cancel::StopReason::UserAbort);
        }
    }
}

/// Pick the correct language half from a bilingual "中文 / English" string.
fn localize(msg: &str) -> String {
    yas::lang::localize(msg)
}

/// Extract a human-readable message from a caught panic payload.
fn panic_message(info: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = info.downcast_ref::<&str>() {
        format!("内部错误: {} / Internal error: {}", s, s)
    } else if let Some(s) = info.downcast_ref::<String>() {
        format!("内部错误: {} / Internal error: {}", s, s)
    } else {
        "内部错误 (未知) / Internal error (unknown)".to_string()
    }
}

/// Spawn a scan operation on a background thread.
pub fn spawn_scan(state: &AppState) -> TaskHandle {
    let status = state.scan_status.clone();
    let user_config = state.user_config.clone();
    let scan_config = state.to_scan_config();
    let lang = state.lang;

    let token = yas::cancel::CancelToken::new();
    let stop_token = token.clone();
    *status.lock().unwrap() = TaskStatus::Running(
        lang.t("正在初始化...", "Initializing...").into(),
    );

    // Check ONNX runtime before spawning
    #[cfg(target_os = "windows")]
    {
        if !yas_genshin::cli::check_onnxruntime() {
            *status.lock().unwrap() = TaskStatus::Running(
                lang.t(
                    "正在下载 ONNX Runtime...",
                    "Downloading ONNX Runtime...",
                )
                .into(),
            );
        }
    }

    let abort_hint = lang.t("鼠标右键终止", "Right-click to abort");

    let handle = thread::spawn(move || {
        let status_for_panic = status.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Ensure ONNX runtime on the worker thread
            #[cfg(target_os = "windows")]
            {
                if !yas_genshin::cli::check_onnxruntime() {
                    if let Err(e) = yas_genshin::cli::download_onnxruntime() {
                        *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e)));
                        return;
                    }
                }
            }

            let status_for_cb = status.clone();
            let status_fn = move |msg: &str| {
                let localized = localize(msg);
                let display = format!("{}  ({})", localized, abort_hint);
                *status_for_cb.lock().unwrap() = TaskStatus::Running(display);
            };

            match yas_genshin::cli::run_scan_core(&user_config, &scan_config, Some(&status_fn), Some(token)) {
                Ok(path) => {
                    let msg = match lang {
                        Lang::Zh => format!("已导出至 {}", path),
                        Lang::En => format!("Exported to {}", path),
                    };
                    *status.lock().unwrap() = TaskStatus::Completed(msg);
                }
                Err(e) => {
                    *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e)));
                }
            }
        }));

        if let Err(panic_info) = result {
            let msg = panic_message(&panic_info);
            log::error!("{}", msg);
            if let Ok(mut guard) = status_for_panic.lock() {
                *guard = TaskStatus::Failed(localize(&msg));
            }
        }
    });

    TaskHandle { _handle: handle, shutdown: None, cancel_token: Some(stop_token) }
}

/// Spawn the HTTP server on a background thread.
pub fn spawn_server(state: &AppState) -> TaskHandle {
    let status = state.server_status.clone();
    let user_config = state.user_config.clone();
    let port = state.server_port;
    let enabled = state.server_enabled.clone();
    let stop_on_all_matched = state.stop_on_all_matched;
    let lang = state.lang;
    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();

    let msg = match lang {
        Lang::Zh => format!("服务器运行中，端口 {}", port),
        Lang::En => format!("Server running on port {}", port),
    };
    *status.lock().unwrap() = TaskStatus::Running(msg);

    let handle = thread::spawn(move || {
        let status_for_panic = status.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Ensure ONNX runtime
            #[cfg(target_os = "windows")]
            {
                if !yas_genshin::cli::check_onnxruntime() {
                    if let Err(e) = yas_genshin::cli::download_onnxruntime() {
                        *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e)));
                        return;
                    }
                }
            }

            match yas_genshin::cli::run_server_core(&user_config, port, None, "ppocrv4", enabled, shutdown_clone, stop_on_all_matched) {
                Ok(()) => {
                    *status.lock().unwrap() = TaskStatus::Completed(
                        lang.t("服务器已停止", "Server stopped").into(),
                    );
                }
                Err(e) => {
                    *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e)));
                }
            }
        }));

        if let Err(panic_info) = result {
            let msg = panic_message(&panic_info);
            log::error!("{}", msg);
            if let Ok(mut guard) = status_for_panic.lock() {
                *guard = TaskStatus::Failed(localize(&msg));
            }
        }
    });

    TaskHandle { _handle: handle, shutdown: Some(shutdown), cancel_token: None }
}

/// Spawn manage-from-JSON on a background thread.
pub fn spawn_manage_json(
    user_config: GoodUserConfig,
    json_str: String,
    status: Arc<Mutex<TaskStatus>>,
    lang: Lang,
) -> TaskHandle {
    *status.lock().unwrap() = TaskStatus::Running(
        lang.t("正在执行管理指令...", "Executing manage instructions...").into(),
    );

    let token = yas::cancel::CancelToken::new();
    let stop_token = token.clone();

    let handle = thread::spawn(move || {
        let status_for_panic = status.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Ensure ONNX runtime
            #[cfg(target_os = "windows")]
            {
                if !yas_genshin::cli::check_onnxruntime() {
                    if let Err(e) = yas_genshin::cli::download_onnxruntime() {
                        *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e)));
                        return;
                    }
                }
            }

            match yas_genshin::cli::run_manage_json(&user_config, &json_str, None, "ppocrv4", Some(token)) {
                Ok(result) => {
                    let s = &result.summary;
                    let msg = match lang {
                        Lang::Zh => format!(
                            "完成: {} 成功, {} 已正确, {} 未找到, {} 错误",
                            s.success, s.already_correct, s.not_found, s.errors
                        ),
                        Lang::En => format!(
                            "Done: {} success, {} already correct, {} not found, {} errors",
                            s.success, s.already_correct, s.not_found, s.errors
                        ),
                    };
                    *status.lock().unwrap() = TaskStatus::Completed(msg);
                }
                Err(e) => {
                    *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e)));
                }
            }
        }));

        if let Err(panic_info) = result {
            let msg = panic_message(&panic_info);
            log::error!("{}", msg);
            if let Ok(mut guard) = status_for_panic.lock() {
                *guard = TaskStatus::Failed(localize(&msg));
            }
        }
    });

    TaskHandle { _handle: handle, shutdown: None, cancel_token: Some(stop_token) }
}
