use std::cell::Cell;
use std::sync::{Arc, Mutex};

use log::{Level, LevelFilter, Log, Metadata, Record};

use super::state::{LogEntry, LogSource};

thread_local! {
    /// Per-thread override for log routing. When set, logs on this thread
    /// (regardless of module path) are routed to the specified source.
    /// Used by `worker::spawn_server` so that logs emitted from `yas_genshin::cli`
    /// during server startup are classified as Manager.
    static LOG_SOURCE_OVERRIDE: Cell<Option<LogSource>> = const { Cell::new(None) };
}

/// Set the log source override for the current thread.
pub fn set_thread_log_source(src: LogSource) {
    LOG_SOURCE_OVERRIDE.with(|c| c.set(Some(src)));
}

fn classify(record: &Record) -> LogSource {
    if let Some(src) = LOG_SOURCE_OVERRIDE.with(|c| c.get()) {
        return src;
    }
    match record.module_path() {
        Some(p)
            if p.starts_with("yas_genshin::manager")
                || p.starts_with("yas_genshin::server") =>
        {
            LogSource::Manager
        }
        _ => LogSource::Scanner,
    }
}

/// Custom logger that routes `log` crate output to per-tab buffers for GUI display,
/// and optionally to a file in the `log/` directory.
pub struct GuiLogger {
    scanner: Arc<Mutex<Vec<LogEntry>>>,
    manager: Arc<Mutex<Vec<LogEntry>>>,
    max_lines: usize,
    log_file: Option<Mutex<std::fs::File>>,
}

impl GuiLogger {
    pub fn new(
        scanner: Arc<Mutex<Vec<LogEntry>>>,
        manager: Arc<Mutex<Vec<LogEntry>>>,
        max_lines: usize,
    ) -> Self {
        // Create log/ directory and open a timestamped log file
        let log_file = std::fs::create_dir_all("log")
            .ok()
            .and_then(|_| {
                let ts = format_timestamp().replace(':', "-");
                std::fs::File::create(format!("log/run_{}.log", ts)).ok()
            })
            .map(Mutex::new);
        Self { scanner, manager, max_lines, log_file }
    }

    pub fn init(self) {
        log::set_boxed_logger(Box::new(self)).unwrap();
        log::set_max_level(LevelFilter::Info);
    }
}

impl Log for GuiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let raw = format!("{}", record.args());
            debug_assert_i18n(record, &raw);
            let localized = yas::lang::localize(&raw);
            let ts = format_timestamp();
            let source = classify(record);
            let entry = LogEntry {
                level: record.level(),
                message: localized.clone(),
                timestamp: ts.clone(),
                source,
            };
            let buf = match source {
                LogSource::Scanner => &self.scanner,
                LogSource::Manager => &self.manager,
            };
            if let Ok(mut lines) = buf.lock() {
                lines.push(entry);
                if lines.len() > self.max_lines {
                    let excess = lines.len() - self.max_lines;
                    lines.drain(0..excess);
                }
            }
            // Also write to log file
            if let Some(ref file_mutex) = self.log_file {
                if let Ok(mut f) = file_mutex.lock() {
                    use std::io::Write;
                    let _ = writeln!(f, "{} [{}] {}", ts, record.level(), localized);
                }
            }
        }
    }

    fn flush(&self) {
        if let Some(ref file_mutex) = self.log_file {
            if let Ok(mut f) = file_mutex.lock() {
                use std::io::Write;
                let _ = f.flush();
            }
        }
    }
}

/// In debug builds, warn about INFO+ log messages missing i18n separator.
///
/// All user-visible log messages (INFO, WARN, ERROR) should use the
/// `"中文 / English"` convention so `localize()` can pick the right language.
/// Messages from third-party crates are excluded (they don't follow our convention).
#[cfg(debug_assertions)]
fn debug_assert_i18n(record: &Record, raw: &str) {
    // Only check our own crates
    let dominated_target = record.target().starts_with("yas")
        || record.target().starts_with("application");
    if !dominated_target {
        return;
    }
    if record.level() <= Level::Info && !raw.contains(" / ") {
        eprintln!(
            "[i18n] missing \" / \" separator in {} message at {}:{}: {:?}",
            record.level(),
            record.file().unwrap_or("?"),
            record.line().unwrap_or(0),
            if raw.len() > 80 { &raw[..80] } else { raw },
        );
    }
}

#[cfg(not(debug_assertions))]
fn debug_assert_i18n(_record: &Record, _raw: &str) {}

#[cfg(windows)]
fn format_timestamp() -> String {
    use std::mem::MaybeUninit;
    #[repr(C)]
    struct SystemTime {
        w_year: u16,
        w_month: u16,
        w_day_of_week: u16,
        w_day: u16,
        w_hour: u16,
        w_minute: u16,
        w_second: u16,
        w_milliseconds: u16,
    }
    extern "system" {
        fn GetLocalTime(lp_system_time: *mut SystemTime);
    }
    let mut st = MaybeUninit::<SystemTime>::uninit();
    unsafe {
        GetLocalTime(st.as_mut_ptr());
        let st = st.assume_init();
        format!("{:02}:{:02}:{:02}", st.w_hour, st.w_minute, st.w_second)
    }
}

#[cfg(not(windows))]
fn format_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let secs_of_day = now % 86400;
    let hours = secs_of_day / 3600;
    let minutes = (secs_of_day % 3600) / 60;
    let seconds = secs_of_day % 60;
    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}
