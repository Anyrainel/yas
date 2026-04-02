use std::sync::{Arc, Mutex};

use log::{Level, LevelFilter, Log, Metadata, Record};

use super::state::LogEntry;

/// Custom logger that routes `log` crate output to a shared buffer for GUI display,
/// and optionally to a file in the `log/` directory.
pub struct GuiLogger {
    lines: Arc<Mutex<Vec<LogEntry>>>,
    max_lines: usize,
    log_file: Option<Mutex<std::fs::File>>,
}

impl GuiLogger {
    pub fn new(lines: Arc<Mutex<Vec<LogEntry>>>, max_lines: usize) -> Self {
        // Create log/ directory and open a timestamped log file
        let log_file = std::fs::create_dir_all("log")
            .ok()
            .and_then(|_| {
                let ts = format_timestamp().replace(':', "-");
                std::fs::File::create(format!("log/gui_{}.log", ts)).ok()
            })
            .map(Mutex::new);
        Self { lines, max_lines, log_file }
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
            let localized = yas::lang::localize(&raw);
            let ts = format_timestamp();
            let entry = LogEntry {
                level: record.level(),
                message: localized.clone(),
                timestamp: ts.clone(),
            };
            if let Ok(mut lines) = self.lines.lock() {
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
