use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use clap::{command, ArgMatches, Args, FromArgMatches};
use log::info;
use serde::{Deserialize, Serialize};

use yas::game_info::{GameInfo, GameInfoBuilder};

use crate::scanner::good_artifact_scanner::{GoodArtifactScanner, GoodArtifactScannerConfig};
use crate::scanner::good_character_scanner::{GoodCharacterScanner, GoodCharacterScannerConfig};
use crate::scanner::good_common::backpack_scanner::BackpackScanner;
use crate::scanner::good_common::constants::*;
use crate::scanner::good_common::diff;
use crate::scanner::good_common::game_controller::GenshinGameController;
use crate::scanner::good_common::mappings::{MappingManager, NameOverrides};
use crate::scanner::good_common::models::{DebugScanResult, GoodExport};
use crate::scanner::good_weapon_scanner::{GoodWeaponScanner, GoodWeaponScannerConfig};

/// Config file name — looked up next to the executable.
const CONFIG_FILE_NAME: &str = "good_config.json";

/// Get the directory containing the running executable.
/// Falls back to current working directory if the exe path can't be determined.
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// User config stored in good_config.json.
///
/// Holds user-specific in-game names so they don't need to be provided
/// via CLI flags every time. CLI flags override values from this file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoodUserConfig {
    /// In-game Traveler name (leave empty if not renamed)
    #[serde(default)]
    pub traveler_name: String,
    /// In-game Wanderer name (leave empty if not renamed)
    #[serde(default)]
    pub wanderer_name: String,
    /// In-game Manekin name (leave empty if not renamed)
    #[serde(default)]
    pub manekin_name: String,
    /// In-game Manekina name (leave empty if not renamed)
    #[serde(default)]
    pub manekina_name: String,
}

impl GoodUserConfig {
    /// Create a template config with empty fields.
    fn template() -> Self {
        Self {
            traveler_name: String::new(),
            wanderer_name: String::new(),
            manekin_name: String::new(),
            manekina_name: String::new(),
        }
    }

    /// Convert a field to Option<String>: empty string → None.
    fn opt(s: &str) -> Option<String> {
        if s.trim().is_empty() {
            None
        } else {
            Some(s.trim().to_string())
        }
    }

    /// Build NameOverrides from this config, with CLI flags taking precedence.
    fn to_overrides(&self, cli: &GoodScannerConfig) -> NameOverrides {
        NameOverrides {
            traveler_name: cli.traveler_name.clone().or_else(|| Self::opt(&self.traveler_name)),
            wanderer_name: cli.wanderer_name.clone().or_else(|| Self::opt(&self.wanderer_name)),
            manekin_name: cli.manekin_name.clone().or_else(|| Self::opt(&self.manekin_name)),
            manekina_name: cli.manekina_name.clone().or_else(|| Self::opt(&self.manekina_name)),
        }
    }
}

/// Load the user config from good_config.json (next to the executable).
/// If the file does not exist, create a template and return Err to signal exit.
fn load_or_create_config() -> Result<GoodUserConfig> {
    let path = exe_dir().join(CONFIG_FILE_NAME);
    println!("[good] Looking for config at: {}", path.display());

    if !path.exists() {
        let template = GoodUserConfig::template();
        let json = serde_json::to_string_pretty(&template)?;
        std::fs::write(&path, &json)?;
        info!("Created config template at: {}", path.display());
        println!();
        println!("=======================================================");
        println!("  Config file created: {}", path.display());
        println!("  Please fill in your in-game character names:");
        println!("    - traveler_name: your Traveler's custom name");
        println!("    - wanderer_name: your Wanderer's custom name");
        println!("    - manekin_name:  your Manekin's custom name");
        println!("    - manekina_name: your Manekina's custom name");
        println!("  Leave empty (\"\") for any you haven't renamed.");
        println!("  Then re-run the scanner.");
        println!("=======================================================");
        println!();
        return Err(anyhow!(
            "Config file created at {}. Fill it in and re-run.",
            path.display()
        ));
    }

    let contents = std::fs::read_to_string(&path)?;
    let config: GoodUserConfig = serde_json::from_str(&contents)
        .map_err(|e| anyhow!("Failed to parse {}: {}", path.display(), e))?;
    info!("Loaded config from {}", path.display());
    Ok(config)
}

#[derive(Clone, clap::Args)]
pub struct GoodScannerConfig {
    /// Scan characters
    #[arg(long = "good-scan-characters", help = "Scan characters")]
    pub scan_characters: bool,

    /// Scan weapons
    #[arg(long = "good-scan-weapons", help = "Scan weapons")]
    pub scan_weapons: bool,

    /// Scan artifacts
    #[arg(long = "good-scan-artifacts", help = "Scan artifacts")]
    pub scan_artifacts: bool,

    /// Scan everything
    #[arg(long = "good-scan-all", help = "Scan all (characters + weapons + artifacts)")]
    pub scan_all: bool,

    /// Output directory
    #[arg(long = "good-output-dir", help = "Output directory", default_value = ".")]
    pub output_dir: String,

    /// Custom Traveler name (if renamed in-game)
    #[arg(long = "good-traveler-name", help = "Custom Traveler name")]
    pub traveler_name: Option<String>,

    /// Custom Wanderer name
    #[arg(long = "good-wanderer-name", help = "Custom Wanderer name")]
    pub wanderer_name: Option<String>,

    /// Custom Manekin name
    #[arg(long = "good-manekin-name", help = "Custom Manekin name")]
    pub manekin_name: Option<String>,

    /// Custom Manekina name
    #[arg(long = "good-manekina-name", help = "Custom Manekina name")]
    pub manekina_name: Option<String>,

    /// OCR backend for all scanners (overrides per-scanner settings)
    #[arg(long = "good-ocr-backend", help = "OCR backend: ppocrv3, ppocrv4, ppocrv5 (default)")]
    pub ocr_backend: Option<String>,

    // === Debug / profiling flags ===

    /// Compare scan output against a groundtruth GOODv3 JSON file
    #[arg(long = "good-debug-compare", help = "Groundtruth GOODv3 JSON path for comparison")]
    pub debug_compare: Option<String>,

    /// For standalone diff mode: path to actual scan output JSON
    #[arg(long = "good-debug-actual", help = "Actual scan JSON path (for offline diff without scanning)")]
    pub debug_actual: Option<String>,

    /// Start scanning from this item index (0-based, weapon/artifact only)
    #[arg(long = "good-debug-start-at", help = "Skip to item index N (0-based)", default_value_t = 0)]
    pub debug_start_at: usize,

    /// For character scanner: jump to the Nth character (0-based)
    #[arg(long = "good-debug-char-index", help = "Jump to character index N (0-based)", default_value_t = 0)]
    pub debug_char_index: usize,

    /// Enable detailed per-field timing output for OCR operations
    #[arg(long = "good-debug-timing", help = "Show per-field OCR timing")]
    pub debug_timing: bool,

    /// Re-scan a specific grid position repeatedly (format: "row,col" 0-indexed)
    #[arg(long = "good-debug-rescan-pos", help = "Re-scan grid position 'row,col' (0-indexed)")]
    pub debug_rescan_pos: Option<String>,

    /// Which scanner to use for re-scan (weapon, artifact, character)
    #[arg(long = "good-debug-rescan-type", help = "Scanner type for re-scan: weapon, artifact, character", default_value = "weapon")]
    pub debug_rescan_type: String,

    /// Number of re-scan iterations (0 = infinite until RMB)
    #[arg(long = "good-debug-rescan-count", help = "Number of re-scan iterations (0=infinite)", default_value_t = 1)]
    pub debug_rescan_count: usize,
}

pub struct GoodScannerApplication {
    arg_matches: ArgMatches,
}

impl GoodScannerApplication {
    pub fn new(matches: ArgMatches) -> Self {
        Self {
            arg_matches: matches,
        }
    }

    pub fn build_command() -> clap::Command {
        let mut cmd = command!();
        cmd = <GoodScannerConfig as Args>::augment_args_for_update(cmd);
        cmd = <GoodCharacterScannerConfig as Args>::augment_args_for_update(cmd);
        cmd = <GoodWeaponScannerConfig as Args>::augment_args_for_update(cmd);
        cmd = <GoodArtifactScannerConfig as Args>::augment_args_for_update(cmd);
        cmd
    }

    fn get_game_info() -> Result<GameInfo> {
        GameInfoBuilder::new()
            .add_local_window_name("\u{539F}\u{795E}") // 原神
            .add_local_window_name("Genshin Impact")
            .add_cloud_window_name("\u{4E91}\u{00B7}\u{539F}\u{795E}") // 云·原神
            .build()
    }

    pub fn run(&self) -> Result<()> {
        println!("[good] GOOD Scanner starting...");
        let arg_matches = &self.arg_matches;
        let config = GoodScannerConfig::from_arg_matches(arg_matches)?;

        // === Standalone diff mode ===
        // When both --good-debug-compare and --good-debug-actual are provided,
        // compare two existing JSON files without launching the game.
        if let (Some(ref compare_path), Some(ref actual_path)) =
            (&config.debug_compare, &config.debug_actual)
        {
            return Self::run_standalone_diff(compare_path, actual_path);
        }

        // === Load user config (good_config.json) ===
        // Creates template and exits if file doesn't exist yet.
        let user_config = load_or_create_config()?;

        // === Re-scan mode ===
        if config.debug_rescan_pos.is_some() {
            return self.run_rescan_mode(&config, &user_config, arg_matches);
        }

        // === Normal scan mode ===
        let game_info = Self::get_game_info()?;

        info!("window: {:?}", game_info.window);
        info!("ui: {:?}", game_info.ui);
        info!("cloud: {}", game_info.is_cloud);

        #[cfg(target_os = "windows")]
        {
            if !yas::utils::is_admin() {
                return Err(anyhow!("Please run as administrator"));
            }
        }

        // Determine what to scan
        let scan_characters = config.scan_characters || config.scan_all;
        let scan_weapons = config.scan_weapons || config.scan_all;
        let scan_artifacts = config.scan_artifacts || config.scan_all
            || (!config.scan_characters && !config.scan_weapons);

        // Fetch and load mappings (user config + CLI overrides)
        info!("=== Loading mappings ===");
        let overrides = user_config.to_overrides(&config);
        if let Some(ref n) = overrides.traveler_name { info!("Traveler name: {}", n); }
        if let Some(ref n) = overrides.wanderer_name { info!("Wanderer name: {}", n); }
        if let Some(ref n) = overrides.manekin_name { info!("Manekin name: {}", n); }
        if let Some(ref n) = overrides.manekina_name { info!("Manekina name: {}", n); }
        let mappings = Rc::new(MappingManager::new(&overrides)?);
        info!(
            "Loaded {} characters, {} weapons, {} artifact sets",
            mappings.character_name_map.len(),
            mappings.weapon_name_map.len(),
            mappings.artifact_set_map.len(),
        );

        // Create shared game controller
        let mut ctrl = GenshinGameController::new(game_info)?;

        // Focus the game window before any keyboard/mouse input.
        // Without this, keys (Escape, B, C) may go to the terminal instead.
        ctrl.focus_game_window();

        let mut characters = None;
        let mut weapons = None;
        let mut artifacts = None;

        // Log OCR backend selection
        if let Some(ref backend) = config.ocr_backend {
            info!("OCR backend override: {}", backend);
        }

        // Scan characters
        if scan_characters {
            info!("=== Scanning characters ===");
            let mut char_config = GoodCharacterScannerConfig::from_arg_matches(arg_matches)?;
            if let Some(ref backend) = config.ocr_backend {
                char_config.ocr_backend = backend.clone();
            }
            let scanner = GoodCharacterScanner::new(
                char_config,
                mappings.clone(),
            )?;
            let t = Instant::now();
            let result = scanner.scan(&mut ctrl, config.debug_char_index)?;
            if config.debug_timing {
                let elapsed = t.elapsed();
                let avg = if result.is_empty() { 0 } else { elapsed.as_millis() as usize / result.len() };
                info!("[timing] characters: {} items in {:?} (avg {}ms/item)", result.len(), elapsed, avg);
            }
            info!("Scanned {} characters", result.len());
            characters = Some(result);

            // Return to main UI before next scan
            ctrl.key_press(enigo::Key::Escape);
            yas::utils::sleep(1000);
        }

        // Scan weapons
        if scan_weapons {
            info!("=== Scanning weapons ===");
            let mut weapon_config = GoodWeaponScannerConfig::from_arg_matches(arg_matches)?;
            if let Some(ref backend) = config.ocr_backend {
                weapon_config.ocr_backend = backend.clone();
            }
            let scanner = GoodWeaponScanner::new(
                weapon_config,
                mappings.clone(),
            )?;
            let t = Instant::now();
            let result = scanner.scan(&mut ctrl, false, config.debug_start_at)?;
            if config.debug_timing {
                let elapsed = t.elapsed();
                let avg = if result.is_empty() { 0 } else { elapsed.as_millis() as usize / result.len() };
                info!("[timing] weapons: {} items in {:?} (avg {}ms/item)", result.len(), elapsed, avg);
            }
            info!("Scanned {} weapons", result.len());
            weapons = Some(result);
        }

        // Scan artifacts
        if scan_artifacts {
            info!("=== Scanning artifacts ===");
            let mut artifact_config = GoodArtifactScannerConfig::from_arg_matches(arg_matches)?;
            if let Some(ref backend) = config.ocr_backend {
                artifact_config.ocr_backend = backend.clone();
            }
            // If weapons were just scanned, we're already in the backpack
            let skip_open = scan_weapons;
            let scanner = GoodArtifactScanner::new(
                artifact_config,
                mappings.clone(),
            )?;
            let t = Instant::now();
            let result = scanner.scan(&mut ctrl, skip_open, config.debug_start_at)?;
            if config.debug_timing {
                let elapsed = t.elapsed();
                let avg = if result.is_empty() { 0 } else { elapsed.as_millis() as usize / result.len() };
                info!("[timing] artifacts: {} items in {:?} (avg {}ms/item)", result.len(), elapsed, avg);
            }
            info!("Scanned {} artifacts", result.len());
            artifacts = Some(result);
        }

        // Export as GOOD v3
        let export = GoodExport::new(characters, weapons, artifacts);
        let json = serde_json::to_string_pretty(&export)?;

        let timestamp = chrono_timestamp();
        let output_dir = PathBuf::from(&config.output_dir);
        std::fs::create_dir_all(&output_dir)?;
        let filename = format!("good_export_{}.json", timestamp);
        let path = output_dir.join(&filename);

        std::fs::write(&path, &json)?;
        info!("Exported to {}", path.display());

        // Post-scan groundtruth comparison
        if let Some(ref compare_path) = config.debug_compare {
            info!("=== Comparing against groundtruth ===");
            let gt_json = std::fs::read_to_string(compare_path)?;
            let groundtruth: GoodExport = serde_json::from_str(&gt_json)?;
            let result = diff::diff_exports(&export, &groundtruth);
            diff::print_diff(&result);

            if result.summary.total_errors() > 0 {
                return Err(anyhow!(
                    "Groundtruth comparison failed: {} errors",
                    result.summary.total_errors()
                ));
            }
            info!("Groundtruth comparison passed!");
        }

        Ok(())
    }

    /// Re-scan mode: click a specific grid position and scan it repeatedly.
    fn run_rescan_mode(
        &self,
        config: &GoodScannerConfig,
        user_config: &GoodUserConfig,
        arg_matches: &ArgMatches,
    ) -> Result<()> {
        let pos_str = config.debug_rescan_pos.as_deref().unwrap();
        let parts: Vec<&str> = pos_str.split(',').collect();
        if parts.len() != 2 {
            return Err(anyhow!("--good-debug-rescan-pos must be 'row,col' (e.g., '2,3')"));
        }
        let row: usize = parts[0].trim().parse()
            .map_err(|_| anyhow!("Invalid row in rescan pos"))?;
        let col: usize = parts[1].trim().parse()
            .map_err(|_| anyhow!("Invalid col in rescan pos"))?;

        info!("=== Re-scan mode: type={} pos=({},{}) count={} ===",
            config.debug_rescan_type, row, col, config.debug_rescan_count);

        let game_info = Self::get_game_info()?;

        #[cfg(target_os = "windows")]
        {
            if !yas::utils::is_admin() {
                return Err(anyhow!("Please run as administrator"));
            }
        }

        let overrides = user_config.to_overrides(config);
        let mappings = Rc::new(MappingManager::new(&overrides)?);
        let mut ctrl = GenshinGameController::new(game_info)?;
        ctrl.focus_game_window();

        let ocr_backend = config.ocr_backend.as_deref().unwrap_or("ppocrv5");

        match config.debug_rescan_type.as_str() {
            "character" => {
                // Character re-scan: open character screen, jump to index, scan
                let mut char_config = GoodCharacterScannerConfig::from_arg_matches(arg_matches)?;
                char_config.ocr_backend = ocr_backend.to_string();
                let scanner = GoodCharacterScanner::new(char_config, mappings.clone())?;

                ctrl.key_press(enigo::Key::Layout('c'));
                yas::utils::sleep(1500);

                // Jump to character index
                if config.debug_char_index > 0 {
                    for _ in 0..config.debug_char_index {
                        ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
                        yas::utils::sleep(200);
                    }
                    yas::utils::sleep(500);
                }

                let max_iter = if config.debug_rescan_count == 0 { usize::MAX } else { config.debug_rescan_count };
                for i in 0..max_iter {
                    if yas::utils::is_rmb_down() {
                        info!("[rescan] interrupted by user");
                        break;
                    }
                    println!("\n--- Re-scan iteration {} ---", i + 1);
                    let result = scanner.debug_scan_current(&mut ctrl);
                    print_debug_result(&result);
                }

                ctrl.key_press(enigo::Key::Escape);
            }
            scan_type => {
                // Weapon/artifact re-scan: open backpack, select tab, scroll, click position
                let tab = match scan_type {
                    "weapon" => "weapon",
                    "artifact" => "artifact",
                    _ => return Err(anyhow!("Unknown rescan type: {}", scan_type)),
                };

                // Open backpack and select tab via BackpackScanner,
                // then grab the scaler and drop bp to release the borrow.
                let scaler = {
                    let mut bp = BackpackScanner::new(&mut ctrl);
                    bp.open_backpack(1000);
                    bp.select_tab(tab, 500);
                    bp.scaler().clone()
                };

                // Scroll to the right page if start_at > 0
                if config.debug_start_at > 0 {
                    let items_per_page = GRID_COLS * GRID_ROWS;
                    let pages_to_skip = config.debug_start_at / items_per_page;
                    if pages_to_skip > 0 {
                        info!("[rescan] scrolling {} pages ({} rows)...", pages_to_skip, pages_to_skip * GRID_ROWS);
                        let estimated_ticks = pages_to_skip * GRID_ROWS * 5;
                        for _ in 0..estimated_ticks {
                            ctrl.mouse_scroll(-1);
                        }
                        yas::utils::sleep(500);
                    }
                }

                // Click the specified position
                let x = GRID_FIRST_X + col as f64 * GRID_OFFSET_X;
                let y = GRID_FIRST_Y + row as f64 * GRID_OFFSET_Y;

                // Create the appropriate scanner
                let max_iter = if config.debug_rescan_count == 0 { usize::MAX } else { config.debug_rescan_count };

                match tab {
                    "weapon" => {
                        let mut weapon_config = GoodWeaponScannerConfig::from_arg_matches(arg_matches)?;
                        weapon_config.ocr_backend = ocr_backend.to_string();
                        let scanner = GoodWeaponScanner::new(weapon_config, mappings.clone())?;

                        for i in 0..max_iter {
                            if yas::utils::is_rmb_down() {
                                info!("[rescan] interrupted by user");
                                break;
                            }
                            println!("\n--- Re-scan iteration {} ---", i + 1);

                            ctrl.move_to(x, y);
                            yas::utils::sleep(50);
                            ctrl.click_at(x, y);
                            yas::utils::sleep(500);

                            let image = ctrl.capture_game()?;
                            let result = scanner.debug_scan_single(&image, &scaler);
                            print_debug_result(&result);
                        }
                    }
                    "artifact" => {
                        let mut artifact_config = GoodArtifactScannerConfig::from_arg_matches(arg_matches)?;
                        artifact_config.ocr_backend = ocr_backend.to_string();
                        let scanner = GoodArtifactScanner::new(artifact_config, mappings.clone())?;

                        for i in 0..max_iter {
                            if yas::utils::is_rmb_down() {
                                info!("[rescan] interrupted by user");
                                break;
                            }
                            println!("\n--- Re-scan iteration {} ---", i + 1);

                            ctrl.move_to(x, y);
                            yas::utils::sleep(50);
                            ctrl.click_at(x, y);
                            yas::utils::sleep(500);

                            let image = ctrl.capture_game()?;
                            let result = scanner.debug_scan_single(&image, &scaler);
                            print_debug_result(&result);
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }

        info!("=== Re-scan complete ===");
        Ok(())
    }

    /// Standalone diff mode: compare two existing JSON files without game.
    fn run_standalone_diff(compare_path: &str, actual_path: &str) -> Result<()> {
        info!("=== Standalone diff mode ===");
        info!("Groundtruth: {}", compare_path);
        info!("Actual:      {}", actual_path);

        let gt_json = std::fs::read_to_string(compare_path)?;
        let groundtruth: GoodExport = serde_json::from_str(&gt_json)?;

        let act_json = std::fs::read_to_string(actual_path)?;
        let actual: GoodExport = serde_json::from_str(&act_json)?;

        let result = diff::diff_exports(&actual, &groundtruth);
        diff::print_diff(&result);

        if result.summary.total_errors() > 0 {
            return Err(anyhow!(
                "Diff found {} errors",
                result.summary.total_errors()
            ));
        }
        info!("Files match!");
        Ok(())
    }
}

/// Print a DebugScanResult to stdout.
fn print_debug_result(result: &DebugScanResult) {
    for field in &result.fields {
        println!(
            "  {:>14}: raw={:?} → {} ({}ms)",
            field.field_name, field.raw_text, field.parsed_value, field.duration_ms
        );
    }
    println!("  Total: {}ms", result.total_duration_ms);
    println!("{}", result.parsed_json);
}

/// Generate a timestamp string like "2024-01-15_12-30-45"
fn chrono_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Simple timestamp without chrono dependency
    let secs_per_day = 86400u64;
    let secs_per_hour = 3600u64;
    let secs_per_min = 60u64;

    // Days since epoch
    let days = now / secs_per_day;
    let remaining = now % secs_per_day;
    let hours = remaining / secs_per_hour;
    let remaining = remaining % secs_per_hour;
    let minutes = remaining / secs_per_min;
    let seconds = remaining % secs_per_min;

    // Calculate year/month/day from days since epoch (simplified)
    let mut y = 1970i32;
    let mut d = days as i32;

    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if d < days_in_year {
            break;
        }
        d -= days_in_year;
        y += 1;
    }

    let months_days: &[i32] = if is_leap(y) {
        &[31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        &[31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut m = 1;
    for &md in months_days {
        if d < md {
            break;
        }
        d -= md;
        m += 1;
    }

    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        y, m, d + 1, hours, minutes, seconds
    )
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
