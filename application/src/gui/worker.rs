use std::sync::Arc;
use std::sync::Mutex;
use std::thread::{self, JoinHandle};

use super::log_bridge;
use super::state::{AppState, Lang, LogSource, TaskStatus};

// ── Windows SEH guard ────────────────────────────────────────────
//
// `catch_unwind` only catches Rust panics.  On Windows, hard crashes
// (access violations, stack overflows, etc.) raise SEH exceptions that
// bypass `catch_unwind` and silently terminate the entire process.
//
// We install a *vectored exception handler* (first-chance, process-wide,
// installed once) that checks a thread-local flag.  If the faulting thread
// is a worker spawned by `spawn_with_safety_net`:
//   1. Write a human-readable error into the shared `TaskStatus`.
//   2. Redirect the instruction pointer to `ExitThread(1)` so only the
//      worker thread terminates — the GUI keeps running.

#[cfg(target_os = "windows")]
mod seh_guard {
    use std::sync::{Arc, Mutex, Once};

    use super::TaskStatus;

    // ── thread-local context set by spawn_with_safety_net ──────────
    thread_local! {
        static SEH_STATUS: std::cell::RefCell<Option<Arc<Mutex<TaskStatus>>>> =
            std::cell::RefCell::new(None);
        static SEH_TASK_NAME: std::cell::RefCell<String> =
            std::cell::RefCell::new(String::new());
    }

    /// Mark the current thread as a guarded worker.
    pub fn set_context(name: &str, status: Arc<Mutex<TaskStatus>>) {
        SEH_STATUS.with(|s| *s.borrow_mut() = Some(status));
        SEH_TASK_NAME.with(|n| *n.borrow_mut() = name.to_string());
    }

    /// Remove the guard (normal exit path).
    pub fn clear_context() {
        SEH_STATUS.with(|s| *s.borrow_mut() = None);
    }

    // ── one-time global handler installation ───────────────────────
    static INSTALLED: Once = Once::new();

    pub fn install_global_handler() {
        INSTALLED.call_once(|| unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::AddVectoredExceptionHandler(
                1, // first-chance (before any frame-based handlers)
                Some(vectored_handler),
            );
        });
    }

    /// Fatal exception codes we intercept.
    fn is_fatal(code: i32) -> bool {
        const ACCESS_VIOLATION: i32 = 0xC0000005_u32 as i32;
        const STACK_OVERFLOW: i32 = 0xC00000FD_u32 as i32;
        const INT_DIVIDE_BY_ZERO: i32 = 0xC0000094_u32 as i32;
        const ILLEGAL_INSTRUCTION: i32 = 0xC000001D_u32 as i32;
        const PRIVILEGED_INSTRUCTION: i32 = 0xC0000096_u32 as i32;
        const HEAP_CORRUPTION: i32 = 0xC0000374_u32 as i32;

        matches!(
            code,
            ACCESS_VIOLATION
                | STACK_OVERFLOW
                | INT_DIVIDE_BY_ZERO
                | ILLEGAL_INSTRUCTION
                | PRIVILEGED_INSTRUCTION
                | HEAP_CORRUPTION
        )
    }

    fn exception_name(code: i32) -> &'static str {
        let u = code as u32;
        match u {
            0xC0000005 => "Access Violation / 访问违规",
            0xC00000FD => "Stack Overflow / 栈溢出",
            0xC0000094 => "Integer Divide by Zero / 整数除零",
            0xC000001D => "Illegal Instruction / 非法指令",
            0xC0000096 => "Privileged Instruction / 特权指令",
            0xC0000374 => "Heap Corruption / 堆损坏",
            _ => "Unknown Exception / 未知异常",
        }
    }

    /// Vectored exception handler callback (called by Windows).
    ///
    /// # Safety
    /// Called by the OS exception dispatcher.  We only touch our own
    /// thread-locals and the provided CONTEXT — both are safe here.
    unsafe extern "system" fn vectored_handler(
        info: *mut windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
    ) -> i32 {
        const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
        const EXCEPTION_CONTINUE_EXECUTION: i32 = -1;

        if info.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }

        let record = (*info).ExceptionRecord;
        if record.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let code = (*record).ExceptionCode;

        if !is_fatal(code) {
            return EXCEPTION_CONTINUE_SEARCH;
        }

        // Check if this thread is a guarded worker.
        //
        // SAFETY constraints inside this handler:
        //   - Use try_lock (not lock) — the thread may have crashed while
        //     holding the status mutex; blocking here would deadlock.
        //   - Use a static description string (no heap allocation) — the
        //     crash may be heap corruption, making `format!` unsafe.
        let handled = SEH_STATUS.with(|s| {
            let guard = s.borrow();
            if let Some(ref status) = *guard {
                let desc = exception_name(code);

                // Best-effort status update — try_lock avoids deadlock if
                // the mutex was held when the crash occurred.
                if let Ok(mut st) = status.try_lock() {
                    if matches!(*st, TaskStatus::Running(_)) {
                        // Use the static description only (no heap-allocating format!).
                        *st = TaskStatus::Failed(
                            yas::lang::localize(desc),
                        );
                    }
                }
                true
            } else {
                false
            }
        });

        if !handled {
            return EXCEPTION_CONTINUE_SEARCH;
        }

        // Redirect execution to ExitThread(1) — kills only this thread.
        let ctx = (*info).ContextRecord;
        if !ctx.is_null() {
            // ExitThread expects one argument (exit code) in RCX on x64.
            (*ctx).Rcx = 1;
            // Align RSP to 16 bytes (Windows x64 ABI requirement).
            (*ctx).Rsp = (*ctx).Rsp & !0xF;
            // Jump to ExitThread.
            (*ctx).Rip = windows_sys::Win32::System::Threading::ExitThread as u64;
        }

        EXCEPTION_CONTINUE_EXECUTION
    }
}

/// Install the global SEH handler.  Call once at startup to protect all
/// threads (including main) against fatal Windows exceptions.
#[cfg(target_os = "windows")]
pub fn install_seh_handler() {
    seh_guard::install_global_handler();
}

/// Run a closure on a background thread with comprehensive error handling:
/// - Catches Rust panics via `catch_unwind`
/// - On Windows, installs a vectored exception handler to catch SEH crashes
///   (access violations, etc.) and convert them to a Failed status.
///
/// The closure should set `status` to Completed/Failed on success/error.
/// This wrapper only intervenes for unexpected crashes.
fn spawn_with_safety_net(
    name: &str,
    log_source: LogSource,
    status: Arc<Mutex<TaskStatus>>,
    f: impl FnOnce(Arc<Mutex<TaskStatus>>) + Send + 'static,
) -> JoinHandle<()> {
    // Ensure the global SEH handler is registered (idempotent).
    #[cfg(target_os = "windows")]
    seh_guard::install_global_handler();

    let task_name = name.to_string();
    thread::spawn(move || {
        log_bridge::set_thread_log_source(log_source);

        // Register this thread for SEH protection.
        #[cfg(target_os = "windows")]
        seh_guard::set_context(&task_name, status.clone());

        let status_for_crash = status.clone();
        let task_name_for_crash = task_name.clone();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            f(status);
        }));

        // Normal exit — remove SEH guard.
        #[cfg(target_os = "windows")]
        seh_guard::clear_context();

        if let Err(panic_info) = result {
            let msg = panic_message(&panic_info);
            yas::log_error!("{} 崩溃: {}", "{} crashed: {}", task_name_for_crash, msg);
            if let Ok(mut guard) = status_for_crash.lock() {
                // Only overwrite if still Running — don't clobber a proper Failed/Completed
                if matches!(*guard, TaskStatus::Running(_)) {
                    *guard = TaskStatus::Failed(localize(&msg));
                }
            }
        }
    })
}

/// Handle to a running background task.
pub struct TaskHandle {
    _handle: JoinHandle<()>,
    /// Optional shutdown flag — set to true to request graceful stop.
    shutdown: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Optional cancel token for scan operations.
    cancel_token: Option<yas::cancel::CancelToken>,
    /// Shared status — used to surface immediate "Stopping..." feedback
    /// when `stop()` is called, without waiting for the worker thread to
    /// notice the cancel token.
    status: Arc<Mutex<TaskStatus>>,
    /// Message shown immediately on stop, in the active UI language.
    stopping_msg: String,
}

impl TaskHandle {
    pub fn is_finished(&self) -> bool {
        self._handle.is_finished()
    }

    /// Whether the underlying cancel token has been tripped (by `stop()`
    /// or by RMB). Lets the UI distinguish "running" from "stopping".
    pub fn is_stopping(&self) -> bool {
        self.cancel_token
            .as_ref()
            .map_or(false, |t| t.is_cancelled())
    }

    /// Signal the task to shut down gracefully.
    ///
    /// Sets the cancel token (the same one RMB uses) and immediately
    /// overwrites the status with a "stopping" message so the user gets
    /// feedback before the worker thread notices.
    pub fn stop(&self) {
        if let Some(ref flag) = self.shutdown {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(ref token) = self.cancel_token {
            token.cancel(yas::cancel::StopReason::UserAbort);
        }
        if let Ok(mut st) = self.status.lock() {
            if matches!(*st, TaskStatus::Running(_)) {
                *st = TaskStatus::Running(self.stopping_msg.clone());
            }
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
    let cancel_for_result = token.clone();
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
    let stopping_msg = lang.t("正在停止扫描...", "Stopping scan...").to_string();

    let handle = spawn_with_safety_net("Scanner", LogSource::Scanner, status.clone(), move |status| {
        // Check VC++ runtime before loading ONNX
        #[cfg(target_os = "windows")]
        {
            if let Err(e) = yas_genshin::cli::check_vcpp_runtime() {
                *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e)));
                return;
            }
        }

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
        // Once the cancel token is tripped, don't let deeper phases
        // overwrite the "Stopping..." message with their own progress.
        let cancel_for_cb = cancel_for_result.clone();
        let stopping_msg_cb = lang.t("正在停止扫描...", "Stopping scan...").to_string();
        let status_fn = move |msg: &str| {
            if cancel_for_cb.is_cancelled() {
                *status_for_cb.lock().unwrap() =
                    TaskStatus::Running(stopping_msg_cb.clone());
                return;
            }
            let localized = localize(msg);
            let display = format!("{}  ({})", localized, abort_hint);
            *status_for_cb.lock().unwrap() = TaskStatus::Running(display);
        };

        let result = yas_genshin::cli::run_scan_core(
            &user_config,
            &scan_config,
            Some(&status_fn),
            Some(token),
        );
        match result {
            Ok(path) => {
                let msg = if cancel_for_result.is_cancelled() {
                    match lang {
                        Lang::Zh => format!("已停止，部分数据已导出至 {}", path),
                        Lang::En => format!("Stopped; partial data exported to {}", path),
                    }
                } else {
                    match lang {
                        Lang::Zh => format!("已导出至 {}", path),
                        Lang::En => format!("Exported to {}", path),
                    }
                };
                *status.lock().unwrap() = TaskStatus::Completed(msg);
            }
            Err(e) => {
                if cancel_for_result.is_cancelled() {
                    // Pre-scan setup (admin check, mappings load, etc.) may
                    // fail immediately after a cancel before any data is
                    // gathered — still surface as a clean stop, not an error.
                    *status.lock().unwrap() = TaskStatus::Completed(
                        lang.t("已停止", "Stopped").into(),
                    );
                } else {
                    *status.lock().unwrap() =
                        TaskStatus::Failed(localize(&format!("{}", e)));
                }
            }
        }
    });

    TaskHandle {
        _handle: handle,
        shutdown: None,
        cancel_token: Some(stop_token),
        status,
        stopping_msg,
    }
}

/// Spawn the HTTP server on a background thread.
pub fn spawn_server(state: &AppState) -> TaskHandle {
    let status = state.server_status.clone();
    let user_config = state.user_config.clone();
    let port = state.server_port;
    let enabled = state.server_enabled.clone();
    let stop_on_all_matched = !state.update_inventory;
    let dump_images = state.dump_images;
    let lang = state.lang;
    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();

    let msg = match lang {
        Lang::Zh => format!("服务器运行中，端口 {}", port),
        Lang::En => format!("Server running on port {}", port),
    };
    *status.lock().unwrap() = TaskStatus::Running(msg);

    let handle = spawn_with_safety_net("Server", LogSource::Manager, status.clone(), move |status| {
        // Check VC++ runtime before loading ONNX
        #[cfg(target_os = "windows")]
        {
            if let Err(e) = yas_genshin::cli::check_vcpp_runtime() {
                *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e)));
                return;
            }
        }

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

        match yas_genshin::cli::run_server_core(&user_config, port, None, "ppocrv4", enabled, shutdown_clone, stop_on_all_matched, dump_images) {
            Ok(()) => {
                *status.lock().unwrap() = TaskStatus::Completed(
                    lang.t("服务器已停止", "Server stopped").into(),
                );
            }
            Err(e) => {
                *status.lock().unwrap() = TaskStatus::Failed(localize(&format!("{}", e)));
            }
        }
    });

    let stopping_msg = lang.t("正在停止服务器...", "Stopping server...").to_string();
    TaskHandle {
        _handle: handle,
        shutdown: Some(shutdown),
        cancel_token: None,
        status,
        stopping_msg,
    }
}
