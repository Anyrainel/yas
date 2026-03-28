use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use super::state::{AppState, Lang, TaskStatus};
use yas_genshin::cli::GoodUserConfig;

/// Handle to a running background task.
pub struct TaskHandle {
    _handle: JoinHandle<()>,
}

impl TaskHandle {
    pub fn is_finished(&self) -> bool {
        self._handle.is_finished()
    }
}

/// Pick the correct language half from a bilingual "中文 / English" string.
fn localize(msg: &str, lang: Lang) -> String {
    if let Some(idx) = msg.find(" / ") {
        match lang {
            Lang::Zh => msg[..idx].to_string(),
            Lang::En => msg[idx + 3..].to_string(),
        }
    } else {
        msg.to_string()
    }
}

/// Spawn a scan operation on a background thread.
pub fn spawn_scan(state: &AppState) -> TaskHandle {
    let status = state.scan_status.clone();
    let user_config = state.user_config.clone();
    let scan_config = state.to_scan_config();
    let lang = state.lang;

    // Reset abort flag for new scan
    yas::utils::reset_abort();
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
        // Ensure ONNX runtime on the worker thread
        #[cfg(target_os = "windows")]
        {
            if !yas_genshin::cli::check_onnxruntime() {
                if let Err(e) = yas_genshin::cli::download_onnxruntime() {
                    *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e), lang));
                    return;
                }
            }
        }

        let status_for_cb = status.clone();
        let status_fn = move |msg: &str| {
            let localized = localize(msg, lang);
            let display = format!("{}  ({})", localized, abort_hint);
            *status_for_cb.lock().unwrap() = TaskStatus::Running(display);
        };

        match yas_genshin::cli::run_scan_core(&user_config, &scan_config, Some(&status_fn)) {
            Ok(path) => {
                let msg = match lang {
                    Lang::Zh => format!("已导出至 {}", path),
                    Lang::En => format!("Exported to {}", path),
                };
                *status.lock().unwrap() = TaskStatus::Completed(msg);
            }
            Err(e) => {
                *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e), lang));
            }
        }
    });

    TaskHandle { _handle: handle }
}

/// Spawn the HTTP server on a background thread.
pub fn spawn_server(state: &AppState) -> TaskHandle {
    let status = state.server_status.clone();
    let user_config = state.user_config.clone();
    let port = state.server_port;
    let enabled = state.server_enabled.clone();
    let lang = state.lang;

    let msg = match lang {
        Lang::Zh => format!("服务器运行中，端口 {}", port),
        Lang::En => format!("Server running on port {}", port),
    };
    *status.lock().unwrap() = TaskStatus::Running(msg);

    let handle = thread::spawn(move || {
        // Ensure ONNX runtime
        #[cfg(target_os = "windows")]
        {
            if !yas_genshin::cli::check_onnxruntime() {
                if let Err(e) = yas_genshin::cli::download_onnxruntime() {
                    *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e), lang));
                    return;
                }
            }
        }

        match yas_genshin::cli::run_server_core(&user_config, port, None, "ppocrv4", enabled) {
            Ok(()) => {
                *status.lock().unwrap() = TaskStatus::Completed(
                    lang.t("服务器已停止", "Server stopped").into(),
                );
            }
            Err(e) => {
                *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e), lang));
            }
        }
    });

    TaskHandle { _handle: handle }
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

    let handle = thread::spawn(move || {
        // Ensure ONNX runtime
        #[cfg(target_os = "windows")]
        {
            if !yas_genshin::cli::check_onnxruntime() {
                if let Err(e) = yas_genshin::cli::download_onnxruntime() {
                    *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e), lang));
                    return;
                }
            }
        }

        match yas_genshin::cli::run_manage_json(&user_config, &json_str, None, "ppocrv4") {
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
                *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e), lang));
            }
        }
    });

    TaskHandle { _handle: handle }
}
