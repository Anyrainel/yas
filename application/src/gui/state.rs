use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use yas_genshin::cli::{GoodUserConfig, ScanCoreConfig};

/// Status of a background operation.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskStatus {
    Idle,
    Running(String),
    Completed(String),
    Failed(String),
}

/// A single log entry displayed in the log panel.
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub level: log::Level,
    pub message: String,
    pub timestamp: String,
}

/// Shared state between GUI thread and background workers.
pub struct AppState {
    // --- Scanner tab config ---
    pub user_config: GoodUserConfig,
    pub scan_characters: bool,
    pub scan_weapons: bool,
    pub scan_artifacts: bool,
    pub weapon_min_rarity: i32,
    pub artifact_min_rarity: i32,
    pub verbose: bool,
    pub continue_on_failure: bool,
    pub dump_images: bool,
    pub output_dir: String,
    pub char_max_count: usize,
    pub weapon_max_count: usize,
    pub artifact_max_count: usize,
    pub weapon_skip_delay: bool,
    pub artifact_skip_delay: bool,

    // --- Scanner task ---
    pub scan_status: Arc<Mutex<TaskStatus>>,

    // --- Manager tab config ---
    pub server_port: u16,
    /// Controls whether POST /manage requests are executed or rejected (503).
    /// Shared with the server thread via Arc.
    pub server_enabled: Arc<AtomicBool>,
    pub server_status: Arc<Mutex<TaskStatus>>,
    pub manage_status: Arc<Mutex<TaskStatus>>,

    // --- Shared log buffer ---
    pub log_lines: Arc<Mutex<Vec<LogEntry>>>,
}

impl AppState {
    pub fn new() -> Self {
        let user_config = yas_genshin::cli::load_config_or_default();
        Self {
            user_config,
            scan_characters: true,
            scan_weapons: true,
            scan_artifacts: true,
            weapon_min_rarity: 3,
            artifact_min_rarity: 4,
            verbose: false,
            continue_on_failure: false,
            dump_images: false,
            output_dir: yas_genshin::cli::exe_dir().display().to_string(),
            char_max_count: 0,
            weapon_max_count: 0,
            artifact_max_count: 0,
            weapon_skip_delay: false,
            artifact_skip_delay: false,
            scan_status: Arc::new(Mutex::new(TaskStatus::Idle)),
            server_port: 8765,
            server_enabled: Arc::new(AtomicBool::new(true)),
            server_status: Arc::new(Mutex::new(TaskStatus::Idle)),
            manage_status: Arc::new(Mutex::new(TaskStatus::Idle)),
            log_lines: Arc::new(Mutex::new(Vec::with_capacity(1000))),
        }
    }

    /// Build a ScanCoreConfig from current UI state.
    pub fn to_scan_config(&self) -> ScanCoreConfig {
        ScanCoreConfig {
            scan_characters: self.scan_characters,
            scan_weapons: self.scan_weapons,
            scan_artifacts: self.scan_artifacts,
            weapon_min_rarity: self.weapon_min_rarity,
            artifact_min_rarity: self.artifact_min_rarity,
            verbose: self.verbose,
            continue_on_failure: self.continue_on_failure,
            log_progress: true,
            dump_images: self.dump_images,
            output_dir: self.output_dir.clone(),
            ocr_backend: None,
            artifact_substat_ocr: "ppocrv4".to_string(),
            char_max_count: self.char_max_count,
            weapon_max_count: self.weapon_max_count,
            artifact_max_count: self.artifact_max_count,
            weapon_skip_delay: self.weapon_skip_delay,
            artifact_skip_delay: self.artifact_skip_delay,
        }
    }
}
