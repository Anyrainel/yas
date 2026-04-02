/// Weapon scanner configuration.
///
/// All fields are set by the orchestrator (`cli.rs`) from global CLI flags
/// and `good_config.json`. This struct has no clap derives.
#[derive(Clone, Debug)]
pub struct GoodWeaponScannerConfig {
    pub min_rarity: i32,
    pub verbose: bool,
    pub ocr_backend: String,
    pub delay_scroll: u64,
    pub delay_tab: u64,
    pub open_delay: u64,
    pub continue_on_failure: bool,
    pub log_progress: bool,
    pub dump_images: bool,
    pub max_count: usize,
    pub capture_delay: u64,
}

impl Default for GoodWeaponScannerConfig {
    fn default() -> Self {
        Self {
            min_rarity: 3,
            verbose: false,
            ocr_backend: "ppocrv4".to_string(),
            delay_scroll: 200,
            delay_tab: 400,
            open_delay: 1200,
            continue_on_failure: false,
            log_progress: false,
            dump_images: false,
            max_count: 0,
            capture_delay: 40,
        }
    }
}
