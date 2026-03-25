use std::sync::{Arc, Mutex};

use log::{Level, LevelFilter, Log, Metadata, Record};

use super::state::LogEntry;

/// Custom logger that routes `log` crate output to a shared buffer for GUI display.
pub struct GuiLogger {
    lines: Arc<Mutex<Vec<LogEntry>>>,
    max_lines: usize,
}

impl GuiLogger {
    pub fn new(lines: Arc<Mutex<Vec<LogEntry>>>, max_lines: usize) -> Self {
        Self { lines, max_lines }
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
            let entry = LogEntry {
                level: record.level(),
                message: format!("{}", record.args()),
                timestamp: format_timestamp(),
            };
            if let Ok(mut lines) = self.lines.lock() {
                lines.push(entry);
                if lines.len() > self.max_lines {
                    let excess = lines.len() - self.max_lines;
                    lines.drain(0..excess);
                }
            }
        }
    }

    fn flush(&self) {}
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
