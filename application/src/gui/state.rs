use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use genshin_scanner::cli::{GoodUserConfig, ScanCoreConfig};

/// UI language.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    pub fn from_str(s: &str) -> Self {
        if s == "en" {
            Lang::En
        } else {
            Lang::Zh
        }
    }

    pub fn to_str(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }

    /// Pick the right string based on current language.
    pub fn t<'a>(self, zh: &'a str, en: &'a str) -> &'a str {
        match self {
            Lang::Zh => zh,
            Lang::En => en,
        }
    }
}

/// Status of a background operation.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskStatus {
    Idle,
    Running(String),
    Completed(String),
    Failed(String),
}

/// Which tab a log entry belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogSource {
    Scanner,
    Manager,
}

/// A single log entry displayed in the log panel.
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub level: log::Level,
    pub message: String,
    pub timestamp: String,
    pub source: LogSource,
}

/// State of the auto-update check.
#[derive(Clone, Debug)]
pub enum UpdateState {
    /// Background check in progress.
    Checking,
    /// A newer version is available.
    Available {
        latest_version: String,
        download_url: String,
    },
    /// Download is in progress.
    Downloading,
    /// Update downloaded and applied — showing restart dialog.
    ShowingDialog,
    /// Update downloaded, user chose to restart later.
    Ready,
    /// Already on the latest version (or dev build).
    None,
    /// Check or download failed (non-fatal).
    Failed(String),
}

/// State of a one-shot data refresh operation.
pub enum RefreshState {
    Idle,
    Running(std::thread::JoinHandle<Result<(), String>>),
    Ok,
    Failed(String),
}

impl RefreshState {
    /// Poll the background thread; transition Running → Ok/Failed when done.
    pub fn poll(&mut self) {
        let finished = matches!(self, RefreshState::Running(h) if h.is_finished());
        if finished {
            let old = std::mem::replace(self, RefreshState::Idle);
            if let RefreshState::Running(h) = old {
                match h.join() {
                    Ok(Ok(())) => *self = RefreshState::Ok,
                    Ok(Err(msg)) => *self = RefreshState::Failed(msg),
                    Err(_) => *self = RefreshState::Failed("thread panicked".into()),
                }
            }
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, RefreshState::Running(_))
    }
}

/// Shared state between GUI thread and background workers.
pub struct AppState {
    // --- Language ---
    pub lang: Lang,

    // --- Auto-update ---
    pub update_state: Arc<Mutex<UpdateState>>,

    // --- Scanner tab config ---
    pub user_config: GoodUserConfig,
    pub scan_characters: bool,
    pub scan_weapons: bool,
    pub scan_artifacts: bool,
    pub verbose: bool,
    pub continue_on_failure: bool,
    pub dump_images: bool,
    pub dump_job_data: bool,
    pub save_on_cancel: bool,
    pub output_dir: String,
    pub char_max_count: usize,
    pub weapon_max_count: usize,
    pub artifact_max_count: usize,

    /// Set to true when Start Scan is pressed but character names are all empty.
    /// Forces the Character Names section open with a warning.
    pub names_need_attention: bool,

    /// Snapshot of user_config for change detection (debounced auto-save).
    pub config_snapshot: String,
    /// When Some, a config change was detected and save is pending after 300ms.
    pub config_dirty_since: Option<Instant>,

    // --- Scanner task ---
    pub scan_status: Arc<Mutex<TaskStatus>>,

    // --- Manager tab config ---
    pub server_port: u16,
    /// Controls whether POST /manage requests are executed or rejected (503).
    /// Shared with the server thread via Arc.
    pub server_enabled: Arc<AtomicBool>,
    /// If true, continue scanning the full inventory after all targets are matched,
    /// providing a complete artifact snapshot via GET /artifacts (slower).
    pub update_inventory: bool,
    pub server_status: Arc<Mutex<TaskStatus>>,
    // --- Per-tab log buffers ---
    pub scanner_log_lines: Arc<Mutex<Vec<LogEntry>>>,
    pub manager_log_lines: Arc<Mutex<Vec<LogEntry>>>,

    // --- Data refresh ---
    pub mappings_refresh: RefreshState,
}

impl AppState {
    pub fn new() -> Self {
        let user_config = genshin_scanner::cli::load_config_or_default();
        let lang = Lang::from_str(&user_config.lang);
        let config_snapshot = serde_json::to_string(&user_config).unwrap_or_default();
        Self {
            lang,
            scan_characters: user_config.scan_characters,
            scan_weapons: user_config.scan_weapons,
            scan_artifacts: user_config.scan_artifacts,
            verbose: user_config.verbose,
            continue_on_failure: user_config.continue_on_failure,
            dump_images: user_config.dump_images,
            dump_job_data: user_config.dump_job_data,
            save_on_cancel: user_config.save_on_cancel,
            char_max_count: user_config.char_max_count,
            weapon_max_count: user_config.weapon_max_count,
            artifact_max_count: user_config.artifact_max_count,
            server_port: user_config.server_port,
            update_inventory: user_config.update_inventory,
            user_config,
            update_state: Arc::new(Mutex::new(UpdateState::Checking)),
            output_dir: genshin_scanner::cli::exe_dir().display().to_string(),
            names_need_attention: false,
            config_snapshot,
            config_dirty_since: None,
            scan_status: Arc::new(Mutex::new(TaskStatus::Idle)),
            server_enabled: Arc::new(AtomicBool::new(true)),
            server_status: Arc::new(Mutex::new(TaskStatus::Idle)),
            scanner_log_lines: Arc::new(Mutex::new(Vec::with_capacity(1000))),
            manager_log_lines: Arc::new(Mutex::new(Vec::with_capacity(1000))),
            mappings_refresh: RefreshState::Idle,
        }
    }

    /// Shorthand for language selection.
    pub fn t<'a>(&self, zh: &'a str, en: &'a str) -> &'a str {
        self.lang.t(zh, en)
    }

    /// Sync GUI fields back into user_config so they get serialized on save.
    fn sync_to_config(&mut self) {
        self.user_config.scan_characters = self.scan_characters;
        self.user_config.scan_weapons = self.scan_weapons;
        self.user_config.scan_artifacts = self.scan_artifacts;
        self.user_config.verbose = self.verbose;
        super::log_bridge::set_verbose(self.verbose);
        self.user_config.continue_on_failure = self.continue_on_failure;
        self.user_config.dump_images = self.dump_images;
        self.user_config.dump_job_data = self.dump_job_data;
        self.user_config.save_on_cancel = self.save_on_cancel;
        self.user_config.char_max_count = self.char_max_count;
        self.user_config.weapon_max_count = self.weapon_max_count;
        self.user_config.artifact_max_count = self.artifact_max_count;
        self.user_config.server_port = self.server_port;
        self.user_config.update_inventory = self.update_inventory;
    }

    /// Check if user_config changed, and if so, schedule a debounced save.
    /// Call this once per frame from the main update loop.
    pub fn auto_save_tick(&mut self) {
        self.sync_to_config();
        let current = serde_json::to_string(&self.user_config).unwrap_or_default();
        if current != self.config_snapshot {
            // Config changed — start/reset the debounce timer
            self.config_dirty_since = Some(Instant::now());
            self.config_snapshot = current;
        }
        if let Some(since) = self.config_dirty_since {
            if since.elapsed() >= std::time::Duration::from_millis(300) {
                if let Err(e) = genshin_scanner::cli::save_config(&self.user_config) {
                    yas::log_warn!("配置自动保存失败: {}", "Config auto-save failed: {}", e);
                }
                self.config_dirty_since = None;
            }
        }
    }

    /// Build a ScanCoreConfig from current UI state.
    pub fn to_scan_config(&self) -> ScanCoreConfig {
        ScanCoreConfig {
            scan_characters: self.scan_characters,
            scan_weapons: self.scan_weapons,
            scan_artifacts: self.scan_artifacts,
            weapon_min_rarity: 3,
            artifact_min_rarity: 4,
            verbose: self.verbose,
            continue_on_failure: self.continue_on_failure,
            log_progress: true,
            dump_images: self.dump_images,
            save_on_cancel: self.save_on_cancel,
            output_dir: self.output_dir.clone(),
            ocr_backend: None,
            artifact_substat_ocr: "ppocrv4".to_string(),
            char_max_count: self.char_max_count,
            weapon_max_count: self.weapon_max_count,
            artifact_max_count: self.artifact_max_count,
        }
    }
}
