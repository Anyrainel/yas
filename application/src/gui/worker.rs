use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use super::state::{AppState, TaskStatus};
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

/// Spawn a scan operation on a background thread.
pub fn spawn_scan(state: &AppState) -> TaskHandle {
    let status = state.scan_status.clone();
    let user_config = state.user_config.clone();
    let scan_config = state.to_scan_config();

    // Reset abort flag for new scan
    yas::utils::reset_abort();
    *status.lock().unwrap() = TaskStatus::Running("正在初始化 / Initializing...".into());

    // Check ONNX runtime before spawning
    #[cfg(target_os = "windows")]
    {
        if !yas_genshin::cli::check_onnxruntime() {
            *status.lock().unwrap() = TaskStatus::Running(
                "正在下载 ONNX Runtime / Downloading ONNX Runtime...".into(),
            );
        }
    }

    let handle = thread::spawn(move || {
        // Ensure ONNX runtime on the worker thread
        #[cfg(target_os = "windows")]
        {
            if !yas_genshin::cli::check_onnxruntime() {
                if let Err(e) = yas_genshin::cli::download_onnxruntime() {
                    *status.lock().unwrap() = TaskStatus::Failed(format!("{}", e));
                    return;
                }
            }
        }

        let status_for_cb = status.clone();
        let status_fn = move |msg: &str| {
            *status_for_cb.lock().unwrap() = TaskStatus::Running(msg.to_string());
        };

        match yas_genshin::cli::run_scan_core(&user_config, &scan_config, Some(&status_fn)) {
            Ok(path) => {
                *status.lock().unwrap() =
                    TaskStatus::Completed(format!("已导出 / Exported to {}", path));
            }
            Err(e) => {
                *status.lock().unwrap() = TaskStatus::Failed(format!("{}", e));
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

    *status.lock().unwrap() =
        TaskStatus::Running(format!("服务器运行中 / Server running on port {}", port));

    let handle = thread::spawn(move || {
        // Ensure ONNX runtime
        #[cfg(target_os = "windows")]
        {
            if !yas_genshin::cli::check_onnxruntime() {
                if let Err(e) = yas_genshin::cli::download_onnxruntime() {
                    *status.lock().unwrap() = TaskStatus::Failed(format!("{}", e));
                    return;
                }
            }
        }

        match yas_genshin::cli::run_server_core(&user_config, port, None, "ppocrv4") {
            Ok(()) => {
                *status.lock().unwrap() = TaskStatus::Completed("服务器已停止 / Server stopped".into());
            }
            Err(e) => {
                *status.lock().unwrap() = TaskStatus::Failed(format!("{}", e));
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
) -> TaskHandle {
    *status.lock().unwrap() = TaskStatus::Running("正在执行管理指令 / Executing manage instructions...".into());

    let handle = thread::spawn(move || {
        // Ensure ONNX runtime
        #[cfg(target_os = "windows")]
        {
            if !yas_genshin::cli::check_onnxruntime() {
                if let Err(e) = yas_genshin::cli::download_onnxruntime() {
                    *status.lock().unwrap() = TaskStatus::Failed(format!("{}", e));
                    return;
                }
            }
        }

        match yas_genshin::cli::run_manage_json(&user_config, &json_str, None, "ppocrv4") {
            Ok(result) => {
                let summary = &result.summary;
                *status.lock().unwrap() = TaskStatus::Completed(format!(
                    "完成 / Done: {} 成功/success, {} 已正确/already correct, {} 未找到/not found, {} 错误/errors",
                    summary.success, summary.already_correct, summary.not_found, summary.errors
                ));
            }
            Err(e) => {
                *status.lock().unwrap() = TaskStatus::Failed(format!("{}", e));
            }
        }
    });

    TaskHandle { _handle: handle }
}
