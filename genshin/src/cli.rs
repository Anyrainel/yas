use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use clap::{command, ArgMatches, Args, FromArgMatches};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};

use yas::game_info::{GameInfo, GameInfoBuilder};

use crate::scanner::artifact::{GoodArtifactScanner, GoodArtifactScannerConfig};
use crate::scanner::character::{GoodCharacterScanner, GoodCharacterScannerConfig};
use crate::scanner::common::backpack_scanner::BackpackScanner;
use crate::scanner::common::constants::*;
use crate::scanner::common::diff;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::{MappingManager, NameOverrides};
use crate::scanner::common::models::{DebugScanResult, GoodExport};
use crate::scanner::common::ocr_factory;
use crate::scanner::weapon::{GoodWeaponScanner, GoodWeaponScannerConfig};

/// Config file path relative to the executable directory.
const CONFIG_FILE_REL: &str = "data/good_config.json";

/// Get the full path to the config file.
pub fn config_path() -> PathBuf {
    exe_dir().join(CONFIG_FILE_REL)
}

/// Get the directory containing the running executable.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

// ================================================================
// ONNX Runtime auto-download
// ================================================================

/// The DLL name that `ort` loads at runtime.
#[cfg(target_os = "windows")]
const ORT_DLL_NAME: &str = "onnxruntime.dll";

/// Mirror URLs to try in order. CDN proxies first (fast globally), GitHub direct last.
#[cfg(target_os = "windows")]
const ORT_DOWNLOAD_URLS: &[&str] = &[
    "https://ghfast.top/https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-win-x64-1.22.0.zip",
    "https://gh-proxy.com/https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-win-x64-1.22.0.zip",
    "https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-win-x64-1.22.0.zip",
];

/// Check if ONNX Runtime is available. Returns true if found, false if download needed.
#[cfg(target_os = "windows")]
pub fn check_onnxruntime() -> bool {
    let dll_path = exe_dir().join(ORT_DLL_NAME);
    if dll_path.exists() {
        std::env::set_var("ORT_DYLIB_PATH", &dll_path);
        true
    } else {
        false
    }
}

/// Download ONNX Runtime without interactive prompts (for GUI mode).
#[cfg(target_os = "windows")]
pub fn download_onnxruntime() -> Result<()> {
    let dll_path = exe_dir().join(ORT_DLL_NAME);
    info!("正在下载 ONNX Runtime... / Downloading ONNX Runtime...");
    download_onnxruntime_inner(&dll_path)
}

#[cfg(target_os = "windows")]
fn download_onnxruntime_inner(dll_path: &std::path::Path) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .connect_timeout(Duration::from_secs(15))
        .build()?;

    let mut last_error = String::new();
    for (i, url) in ORT_DOWNLOAD_URLS.iter().enumerate() {
        info!("尝试源 {} / Trying source {}:  {}", i + 1, i + 1, url);

        match client.get(*url).send() {
            Ok(response) => {
                if !response.status().is_success() {
                    last_error = format!("HTTP {}", response.status());
                    warn!("源 {} 失败 / Source {} failed: {}", i + 1, i + 1, last_error);
                    continue;
                }
                match response.bytes() {
                    Ok(bytes) => {
                        info!(
                            "下载完成（{}字节），正在解压... / Downloaded ({} bytes), extracting...",
                            bytes.len(), bytes.len()
                        );
                        match extract_onnxruntime_dll(&bytes, dll_path) {
                            Ok(()) => {
                                info!("ONNX Runtime 已安装到 / installed to: {}", dll_path.display());
                                std::env::set_var("ORT_DYLIB_PATH", dll_path);
                                return Ok(());
                            }
                            Err(e) => {
                                last_error = format!("{}", e);
                                warn!("解压失败 / Extract failed: {}", last_error);
                                let _ = std::fs::remove_file(dll_path);
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        last_error = format!("{}", e);
                        warn!("下载失败 / Download failed: {}", last_error);
                        continue;
                    }
                }
            }
            Err(e) => {
                last_error = format!("{}", e);
                warn!("连接失败 / Connection failed: {}", last_error);
                continue;
            }
        }
    }

    Err(anyhow!(
        "所有下载源均失败 / All download sources failed: {}\n\
         手动下载地址 / Manual download: {}",
        last_error,
        ORT_DOWNLOAD_URLS.last().unwrap()
    ))
}

/// Ensure onnxruntime.dll is available next to the exe; if not, offer to download it.
///
/// When the DLL exists locally, sets `ORT_DYLIB_PATH` so `ort` uses our copy
/// instead of any older/incompatible system DLL that might be on PATH.
#[cfg(target_os = "windows")]
fn ensure_onnxruntime() -> Result<()> {
    if check_onnxruntime() {
        return Ok(());
    }

    println!();
    println!("=======================================================");
    println!("  未找到 {} / {} not found", ORT_DLL_NAME, ORT_DLL_NAME);
    println!("=======================================================");
    println!();
    println!("OCR引擎需要ONNX Runtime运行库。");
    println!("The OCR engine requires the ONNX Runtime library.");
    println!();
    println!("按回车自动下载（约70MB），或按 Ctrl+C 退出。");
    println!("Press Enter to download automatically (~70MB), or Ctrl+C to exit.");
    let _ = std::io::stdin().read_line(&mut String::new());

    download_onnxruntime()
}

/// Extract onnxruntime.dll from the downloaded zip archive.
#[cfg(target_os = "windows")]
fn extract_onnxruntime_dll(zip_bytes: &[u8], dest: &std::path::Path) -> Result<()> {
    use std::io::{Cursor, Read};
    let reader = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| anyhow!("无法打开压缩包 / Cannot open zip archive: {}", e))?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)
            .map_err(|e| anyhow!("无法读取压缩包条目 / Cannot read zip entry: {}", e))?;
        let name = file.name().to_string();
        if name.ends_with("lib/onnxruntime.dll") {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            std::fs::write(dest, &buf)?;
            return Ok(());
        }
    }

    Err(anyhow!("压缩包中未找到 onnxruntime.dll / onnxruntime.dll not found in zip archive"))
}

// ================================================================
// User config (good_config.json)
// ================================================================

fn default_char_tab_delay() -> u64 { DEFAULT_DELAY_CHAR_TAB_SWITCH }
fn default_open_delay() -> u64 { DEFAULT_DELAY_OPEN_SCREEN }
fn default_grid_delay() -> u64 { DEFAULT_DELAY_GRID_ITEM }
fn default_scroll_delay() -> u64 { DEFAULT_DELAY_SCROLL }
fn default_tab_delay() -> u64 { DEFAULT_DELAY_INV_TAB_SWITCH }

/// User config stored in `data/good_config.json`.
///
/// Holds user-specific in-game names and scanner timing settings.
/// Created interactively on first run; subsequent runs read from the file.
/// New fields are added with serde defaults so old config files still load.
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

    // --- Timing / delay settings ---

    #[serde(default = "default_char_tab_delay")]
    pub char_tab_delay: u64,
    #[serde(default = "default_open_delay")]
    pub char_open_delay: u64,

    #[serde(default = "default_grid_delay")]
    pub weapon_grid_delay: u64,
    #[serde(default = "default_scroll_delay")]
    pub weapon_scroll_delay: u64,
    #[serde(default = "default_tab_delay")]
    pub weapon_tab_delay: u64,
    #[serde(default = "default_open_delay")]
    pub weapon_open_delay: u64,

    #[serde(default = "default_grid_delay")]
    pub artifact_grid_delay: u64,
    #[serde(default = "default_scroll_delay")]
    pub artifact_scroll_delay: u64,
    #[serde(default = "default_tab_delay")]
    pub artifact_tab_delay: u64,
    #[serde(default = "default_open_delay")]
    pub artifact_open_delay: u64,
}

impl GoodUserConfig {
    fn opt(s: &str) -> Option<String> {
        if s.trim().is_empty() { None } else { Some(s.trim().to_string()) }
    }

    fn to_overrides(&self) -> NameOverrides {
        NameOverrides {
            traveler_name: Self::opt(&self.traveler_name),
            wanderer_name: Self::opt(&self.wanderer_name),
            manekin_name: Self::opt(&self.manekin_name),
            manekina_name: Self::opt(&self.manekina_name),
        }
    }
}

impl Default for GoodUserConfig {
    fn default() -> Self {
        Self {
            traveler_name: String::new(),
            wanderer_name: String::new(),
            manekin_name: String::new(),
            manekina_name: String::new(),
            char_tab_delay: default_char_tab_delay(),
            char_open_delay: default_open_delay(),
            weapon_grid_delay: default_grid_delay(),
            weapon_scroll_delay: default_scroll_delay(),
            weapon_tab_delay: default_tab_delay(),
            weapon_open_delay: default_open_delay(),
            artifact_grid_delay: default_grid_delay(),
            artifact_scroll_delay: default_scroll_delay(),
            artifact_tab_delay: default_tab_delay(),
            artifact_open_delay: default_open_delay(),
        }
    }
}

/// Read a single line from stdin, trimming whitespace.
fn prompt_input(label: &str) -> String {
    print!("{}", label);
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
    buf.trim().to_string()
}

/// Run interactive first-run prompts and return a populated config.
fn prompt_first_run_config() -> GoodUserConfig {
    println!();
    println!("=======================================================");
    println!("  首次运行配置 / First-run Configuration");
    println!("=======================================================");
    println!();
    println!("以下角色可以在游戏内自定义名字，请输入您的游戏内名字。");
    println!("The following characters have customizable in-game names.");
    println!("Please enter your in-game names for them.");
    println!("留空表示使用默认名字。/ Leave empty for default names.");
    println!();

    let traveler_name = prompt_input("旅行者 / Traveler: ");
    let wanderer_name = prompt_input("流浪者 / Wanderer: ");
    let manekin_name = prompt_input("奇偶·男性 / Manekin: ");
    let manekina_name = prompt_input("奇偶·女性 / Manekina: ");

    println!();
    println!("请确认游戏已运行，按回车开始扫描。扫描过程中可按鼠标右键终止。");
    println!("Please confirm the game is running. Press Enter to start.");
    println!("You can right-click to abort during scanning.");
    let _ = std::io::stdin().read_line(&mut String::new());

    GoodUserConfig {
        traveler_name,
        wanderer_name,
        manekin_name,
        manekina_name,
        ..Default::default()
    }
}

/// Load the user config from data/good_config.json without interactive prompts.
/// Returns defaults if the file does not exist.
pub fn load_config_or_default() -> GoodUserConfig {
    let path = config_path();
    if path.exists() {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(config) = serde_json::from_str(&contents) {
                return config;
            }
        }
    }
    GoodUserConfig::default()
}

/// Save the user config to data/good_config.json.
pub fn save_config(config: &GoodUserConfig) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, &json)?;
    Ok(())
}

/// Load the user config from data/good_config.json (next to the executable).
/// If the file does not exist, interactively prompt the user and save.
fn load_or_create_config() -> Result<GoodUserConfig> {
    let path = exe_dir().join(CONFIG_FILE_REL);
    println!("正在查找配置文件... / Looking for config at: {}", path.display());

    if !path.exists() {
        let config = prompt_first_run_config();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&config)?;
        std::fs::write(&path, &json)?;
        println!("配置已保存 / Config saved to: {}", path.display());
        debug!("Created config at: {}", path.display());
        return Ok(config);
    }

    let contents = std::fs::read_to_string(&path)?;
    let config: GoodUserConfig = serde_json::from_str(&contents)
        .map_err(|e| anyhow!("配置解析失败 / Failed to parse {}: {}", path.display(), e))?;
    debug!("Loaded config from {}", path.display());

    // Re-save to add any new default fields that didn't exist in the old file
    let updated_json = serde_json::to_string_pretty(&config)?;
    if updated_json != contents {
        let _ = std::fs::write(&path, &updated_json);
        debug!("Config updated with new default fields");
    }

    Ok(config)
}

// ================================================================
// CLI config
// ================================================================

#[derive(Clone, clap::Args)]
#[command(about = "原神GOOD格式扫描器 / Genshin Impact GOOD Format Scanner")]
pub struct GoodScannerConfig {
    // === Scan targets ===

    /// 扫描角色 / Scan characters
    #[arg(long = "characters", help = "扫描角色\nScan characters",
          help_heading = "扫描目标 / Scan Targets")]
    pub scan_characters: bool,

    /// 扫描武器 / Scan weapons
    #[arg(long = "weapons", help = "扫描武器\nScan weapons",
          help_heading = "扫描目标 / Scan Targets")]
    pub scan_weapons: bool,

    /// 扫描圣遗物 / Scan artifacts
    #[arg(long = "artifacts", help = "扫描圣遗物\nScan artifacts",
          help_heading = "扫描目标 / Scan Targets")]
    pub scan_artifacts: bool,

    /// 扫描全部 / Scan all
    #[arg(long = "all", help = "扫描全部（角色+武器+圣遗物）\nScan all (characters + weapons + artifacts)",
          help_heading = "扫描目标 / Scan Targets")]
    pub scan_all: bool,

    // === Global options ===

    /// 显示详细扫描信息 / Show detailed scan info
    #[arg(long = "verbose", short = 'v', help = "显示详细扫描信息\nShow detailed scan info",
          help_heading = "通用选项 / Global Options")]
    pub verbose: bool,

    /// 单项失败时继续扫描 / Continue when items fail
    #[arg(long = "continue-on-failure", help = "单项失败时继续扫描\nContinue scanning when individual items fail",
          help_heading = "通用选项 / Global Options")]
    pub continue_on_failure: bool,

    /// 逐项显示扫描进度 / Log each scanned item
    #[arg(long = "log-progress", help = "逐项显示扫描进度\nLog each item as it is scanned",
          help_heading = "通用选项 / Global Options")]
    pub log_progress: bool,

    /// 输出目录 / Output directory
    #[arg(long = "output-dir", help = "输出目录\nOutput directory", default_value = ".",
          help_heading = "通用选项 / Global Options")]
    pub output_dir: String,

    /// 覆盖OCR后端 / Override OCR backend
    #[arg(long = "ocr-backend", help = "覆盖OCR后端（ppocrv4 或 ppocrv5）\nOverride OCR backend (ppocrv4 or ppocrv5)",
          help_heading = "通用选项 / Global Options")]
    pub ocr_backend: Option<String>,

    /// 保存OCR区域截图 / Dump OCR screenshots
    #[arg(long = "dump-images", help = "保存OCR区域截图到 debug_images/\nDump OCR region screenshots to debug_images/",
          help_heading = "通用选项 / Global Options")]
    pub dump_images: bool,

    // === Scanner config ===

    /// 最低武器稀有度 / Min weapon rarity
    #[arg(long = "weapon-min-rarity", help = "保留的最低武器稀有度（3-5）\nMinimum weapon rarity to keep (3-5)",
          default_value_t = 3, help_heading = "扫描器配置 / Scanner Config")]
    pub weapon_min_rarity: i32,

    /// 最低圣遗物稀有度 / Min artifact rarity
    #[arg(long = "artifact-min-rarity", help = "保留的最低圣遗物稀有度（4-5）\nMinimum artifact rarity to keep (4-5)",
          default_value_t = 4, help_heading = "扫描器配置 / Scanner Config")]
    pub artifact_min_rarity: i32,

    /// 最大角色扫描数 / Max characters
    #[arg(long = "char-max-count", help = "最大角色扫描数（0=不限）\nMax characters to scan (0 = unlimited)",
          default_value_t = 0, help_heading = "扫描器配置 / Scanner Config")]
    pub char_max_count: usize,

    /// 最大武器扫描数 / Max weapons
    #[arg(long = "weapon-max-count", help = "最大武器扫描数（0=不限）\nMax weapons to scan (0 = unlimited)",
          default_value_t = 0, help_heading = "扫描器配置 / Scanner Config")]
    pub weapon_max_count: usize,

    /// 最大圣遗物扫描数 / Max artifacts
    #[arg(long = "artifact-max-count", help = "最大圣遗物扫描数（0=不限）\nMax artifacts to scan (0 = unlimited)",
          default_value_t = 0, help_heading = "扫描器配置 / Scanner Config")]
    pub artifact_max_count: usize,

    /// 跳过武器面板等待 / Skip weapon panel delay
    #[arg(long = "weapon-skip-delay", help = "跳过武器面板等待（加速扫描，锁定检测可能不准）\nSkip weapon panel delay (faster but lock detection may be inaccurate)",
          help_heading = "扫描器配置 / Scanner Config")]
    pub weapon_skip_delay: bool,

    /// 跳过圣遗物面板等待 / Skip artifact panel delay
    #[arg(long = "artifact-skip-delay", help = "跳过圣遗物面板等待（加速扫描，锁定/星辉检测可能不准）\nSkip artifact panel delay (faster but lock/astral detection may be inaccurate)",
          help_heading = "扫描器配置 / Scanner Config")]
    pub artifact_skip_delay: bool,

    /// 圣遗物副词条OCR后端 / Artifact substat OCR backend
    #[arg(long = "artifact-substat-ocr", help = "圣遗物副词条OCR后端\nArtifact substat/general OCR backend",
          default_value = "ppocrv4", help_heading = "扫描器配置 / Scanner Config")]
    pub artifact_substat_ocr: String,

    // === Server mode ===

    /// 启动管理服务器 / Start artifact manager server
    #[arg(long = "server", help = "启动圣遗物管理HTTP服务器（而非扫描模式）\nStart artifact manager HTTP server instead of scanning",
          help_heading = "服务器模式 / Server Mode")]
    pub server_mode: bool,

    /// 服务器端口 / Server port
    #[arg(long = "port", help = "管理服务器监听端口\nArtifact manager server listen port",
          default_value_t = 8765, help_heading = "服务器模式 / Server Mode")]
    pub server_port: u16,

    // === Debug ===

    /// 真值对比 / Groundtruth comparison
    #[arg(long = "debug-compare", help = "与真值JSON文件比较\nGroundtruth GOODv3 JSON path for comparison",
          help_heading = "调试选项 / Debug")]
    pub debug_compare: Option<String>,

    /// 扫描结果JSON / Actual scan JSON
    #[arg(long = "debug-actual", help = "实际扫描JSON路径（离线对比模式）\nActual scan JSON path (for offline diff without scanning)",
          help_heading = "调试选项 / Debug")]
    pub debug_actual: Option<String>,

    /// 从第N项开始 / Start at index
    #[arg(long = "debug-start-at", help = "从第N项开始扫描（0起始）\nSkip to item index N (0-based)",
          default_value_t = 0, help_heading = "调试选项 / Debug")]
    pub debug_start_at: usize,

    /// 跳转角色 / Jump to character index
    #[arg(long = "debug-char-index", help = "跳转到第N个角色（0起始）\nJump to character index N (0-based)",
          default_value_t = 0, help_heading = "调试选项 / Debug")]
    pub debug_char_index: usize,

    /// OCR计时 / Show OCR timing
    #[arg(long = "debug-timing", help = "显示每个字段的OCR耗时\nShow per-field OCR timing",
          help_heading = "调试选项 / Debug")]
    pub debug_timing: bool,

    /// 重复扫描位置 / Re-scan position
    #[arg(long = "debug-rescan-pos", help = "重复扫描指定格子位置 'row,col'（0起始）\nRe-scan grid position 'row,col' (0-indexed)",
          help_heading = "调试选项 / Debug")]
    pub debug_rescan_pos: Option<String>,

    /// 重复扫描类型 / Re-scan type
    #[arg(long = "debug-rescan-type", help = "重复扫描类型: weapon, artifact, character\nScanner type for re-scan",
          default_value = "weapon", help_heading = "调试选项 / Debug")]
    pub debug_rescan_type: String,

    /// 重复扫描次数 / Re-scan count
    #[arg(long = "debug-rescan-count", help = "重复扫描次数（0=无限直到右键）\nNumber of re-scan iterations (0 = infinite until RMB)",
          default_value_t = 1, help_heading = "调试选项 / Debug")]
    pub debug_rescan_count: usize,
}

// ================================================================
// Application
// ================================================================

pub struct GoodScannerApplication {
    arg_matches: ArgMatches,
}

impl GoodScannerApplication {
    pub fn new(matches: ArgMatches) -> Self {
        Self { arg_matches: matches }
    }

    pub fn build_command() -> clap::Command {
        let cmd = command!();
        <GoodScannerConfig as Args>::augment_args_for_update(cmd)
    }

    pub fn get_game_info() -> Result<GameInfo> {
        GameInfoBuilder::new()
            .add_local_window_name("\u{539F}\u{795E}") // 原神
            .add_local_window_name("Genshin Impact")
            .add_cloud_window_name("\u{4E91}\u{00B7}\u{539F}\u{795E}") // 云·原神
            .build()
    }

    /// Build a character scanner config from global CLI flags + JSON config.
    pub fn make_char_config(config: &GoodScannerConfig, user_config: &GoodUserConfig) -> GoodCharacterScannerConfig {
        GoodCharacterScannerConfig {
            verbose: config.verbose,
            ocr_backend: config.ocr_backend.clone().unwrap_or_else(|| "ppocrv4".to_string()),
            tab_delay: user_config.char_tab_delay,
            open_delay: user_config.char_open_delay,
            continue_on_failure: config.continue_on_failure,
            log_progress: config.log_progress,
            dump_images: config.dump_images,
            max_count: config.char_max_count,
        }
    }

    /// Build a weapon scanner config from global CLI flags + JSON config.
    pub fn make_weapon_config(config: &GoodScannerConfig, user_config: &GoodUserConfig) -> GoodWeaponScannerConfig {
        GoodWeaponScannerConfig {
            min_rarity: config.weapon_min_rarity,
            verbose: config.verbose,
            ocr_backend: config.ocr_backend.clone().unwrap_or_else(|| "ppocrv4".to_string()),
            delay_grid_item: user_config.weapon_grid_delay,
            delay_scroll: user_config.weapon_scroll_delay,
            delay_tab: user_config.weapon_tab_delay,
            open_delay: user_config.weapon_open_delay,
            continue_on_failure: config.continue_on_failure,
            log_progress: config.log_progress,
            dump_images: config.dump_images,
            max_count: config.weapon_max_count,
            skip_lock_delay: config.weapon_skip_delay,
        }
    }

    /// Build an artifact scanner config from global CLI flags + JSON config.
    pub fn make_artifact_config(config: &GoodScannerConfig, user_config: &GoodUserConfig) -> GoodArtifactScannerConfig {
        GoodArtifactScannerConfig {
            min_rarity: config.artifact_min_rarity,
            verbose: config.verbose,
            ocr_backend: config.ocr_backend.clone().unwrap_or_else(|| "ppocrv5".to_string()),
            substat_ocr_backend: config.artifact_substat_ocr.clone(),
            delay_grid_item: user_config.artifact_grid_delay,
            delay_scroll: user_config.artifact_scroll_delay,
            delay_tab: user_config.artifact_tab_delay,
            open_delay: user_config.artifact_open_delay,
            continue_on_failure: config.continue_on_failure,
            log_progress: config.log_progress,
            dump_images: config.dump_images,
            max_count: config.artifact_max_count,
            skip_lock_delay: config.artifact_skip_delay,
        }
    }

    pub fn run(&self) -> Result<()> {
        println!("正在启动扫描器... / GOOD Scanner starting...");

        // Check for ONNX Runtime before doing anything else
        #[cfg(target_os = "windows")]
        ensure_onnxruntime()?;

        let arg_matches = &self.arg_matches;
        let config = GoodScannerConfig::from_arg_matches(arg_matches)?;

        // === Standalone diff mode ===
        if let (Some(ref compare_path), Some(ref actual_path)) =
            (&config.debug_compare, &config.debug_actual)
        {
            return Self::run_standalone_diff(compare_path, actual_path);
        }

        // === Load user config (good_config.json) ===
        let user_config = load_or_create_config()?;

        // === Server mode (artifact manager) ===
        if config.server_mode {
            return self.run_server_mode(&config, &user_config);
        }

        // === Re-scan mode ===
        if config.debug_rescan_pos.is_some() {
            return self.run_rescan_mode(&config, &user_config);
        }

        // === Normal scan mode ===

        #[cfg(target_os = "windows")]
        {
            if !yas::utils::is_admin() {
                return Err(anyhow!("请以管理员身份运行 / Please run as administrator"));
            }
        }

        // Determine what to scan (default: all if no flags specified)
        let no_flags = !config.scan_characters && !config.scan_weapons && !config.scan_artifacts && !config.scan_all;
        let scan_characters = config.scan_characters || config.scan_all || no_flags;
        let scan_weapons = config.scan_weapons || config.scan_all || no_flags;
        let scan_artifacts = config.scan_artifacts || config.scan_all || no_flags;

        // Fetch and load mappings (before game interaction — no focus steal)
        info!("=== 加载映射数据 / Loading mappings ===");
        let overrides = user_config.to_overrides();
        if let Some(ref n) = overrides.traveler_name { debug!("旅行者 / Traveler: {}", n); }
        if let Some(ref n) = overrides.wanderer_name { debug!("流浪者 / Wanderer: {}", n); }
        if let Some(ref n) = overrides.manekin_name { debug!("奇偶·男 / Manekin: {}", n); }
        if let Some(ref n) = overrides.manekina_name { debug!("奇偶·女 / Manekina: {}", n); }
        let mappings = Arc::new(MappingManager::new(&overrides)?);
        info!(
            "已加载 / Loaded: {} characters, {} weapons, {} artifact sets",
            mappings.character_name_map.len(),
            mappings.weapon_name_map.len(),
            mappings.artifact_set_map.len(),
        );

        // Find and focus the game window
        let game_info = Self::get_game_info()?;
        debug!("window: {:?}", game_info.window);
        debug!("ui: {:?}", game_info.ui);
        debug!("cloud: {}", game_info.is_cloud);

        let mut ctrl = GenshinGameController::new(game_info)?;
        ctrl.focus_game_window();

        let mut characters = None;
        let mut weapons = None;
        let mut artifacts = None;

        // Log OCR backend selection
        if let Some(ref backend) = config.ocr_backend {
            debug!("OCR后端覆盖 / OCR backend override: {}", backend);
        }

        // Scan characters
        if scan_characters {
            info!("=== 扫描角色 / Scanning characters ===");
            let char_config = Self::make_char_config(&config, &user_config);
            let scanner = GoodCharacterScanner::new(
                char_config, mappings.clone(),
            )?;
            let t = Instant::now();
            let result = scanner.scan(&mut ctrl, config.debug_char_index)?;
            if config.debug_timing {
                let elapsed = t.elapsed();
                let avg = if result.is_empty() { 0 } else { elapsed.as_millis() as usize / result.len() };
                debug!("[timing] characters: {} items in {:?} (avg {}ms/item)", result.len(), elapsed, avg);
            }
            info!("已扫描 / Scanned {} characters", result.len());
            characters = Some(result);

            if !yas::utils::was_aborted() {
                ctrl.return_to_main_ui(4);
            }
        }

        // Scan weapons
        if scan_weapons && !yas::utils::was_aborted() {
            info!("=== 扫描武器 / Scanning weapons ===");
            let weapon_config = Self::make_weapon_config(&config, &user_config);
            let scanner = GoodWeaponScanner::new(
                weapon_config, mappings.clone(),
            )?;
            let t = Instant::now();
            let result = scanner.scan(&mut ctrl, false, config.debug_start_at)?;
            if config.debug_timing {
                let elapsed = t.elapsed();
                let avg = if result.is_empty() { 0 } else { elapsed.as_millis() as usize / result.len() };
                debug!("[timing] weapons: {} items in {:?} (avg {}ms/item)", result.len(), elapsed, avg);
            }
            info!("已扫描 / Scanned {} weapons", result.len());
            weapons = Some(result);
        }

        // Scan artifacts
        if scan_artifacts && !yas::utils::was_aborted() {
            info!("=== 扫描圣遗物 / Scanning artifacts ===");
            let artifact_config = Self::make_artifact_config(&config, &user_config);
            let skip_open = scan_weapons;
            let scanner = GoodArtifactScanner::new(
                artifact_config, mappings.clone(),
            )?;
            let t = Instant::now();
            let result = scanner.scan(&mut ctrl, skip_open, config.debug_start_at)?;
            if config.debug_timing {
                let elapsed = t.elapsed();
                let avg = if result.is_empty() { 0 } else { elapsed.as_millis() as usize / result.len() };
                debug!("[timing] artifacts: {} items in {:?} (avg {}ms/item)", result.len(), elapsed, avg);
            }
            info!("已扫描 / Scanned {} artifacts", result.len());
            artifacts = Some(result);
        }

        if yas::utils::was_aborted() {
            info!("扫描被用户中断 / Scan aborted by user (right-click)");
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
        info!("已导出 / Exported to {}", path.display());

        // Post-scan groundtruth comparison
        if let Some(ref compare_path) = config.debug_compare {
            info!("=== 真值对比 / Comparing against groundtruth ===");
            let gt_json = std::fs::read_to_string(compare_path)?;
            let groundtruth: GoodExport = serde_json::from_str(&gt_json)?;
            let result = diff::diff_exports(&export, &groundtruth);
            diff::print_diff(&result);

            if result.summary.total_errors() > 0 {
                return Err(anyhow!(
                    "真值对比失败 / Groundtruth comparison failed: {} errors",
                    result.summary.total_errors()
                ));
            }
            info!("真值对比通过 / Groundtruth comparison passed!");
        }

        Ok(())
    }

    /// Re-scan mode: click a specific grid position and scan it repeatedly.
    fn run_rescan_mode(
        &self,
        config: &GoodScannerConfig,
        user_config: &GoodUserConfig,
    ) -> Result<()> {
        let pos_str = config.debug_rescan_pos.as_deref().unwrap();
        let parts: Vec<&str> = pos_str.split(',').collect();
        if parts.len() != 2 {
            return Err(anyhow!("--debug-rescan-pos 格式应为 'row,col'（例如 '2,3'）\n--debug-rescan-pos must be 'row,col' (e.g., '2,3')"));
        }
        let row: usize = parts[0].trim().parse()
            .map_err(|_| anyhow!("无效的行号 / Invalid row in rescan pos"))?;
        let col: usize = parts[1].trim().parse()
            .map_err(|_| anyhow!("无效的列号 / Invalid col in rescan pos"))?;

        info!("=== Re-scan mode: type={} pos=({},{}) count={} ===",
            config.debug_rescan_type, row, col, config.debug_rescan_count);

        let game_info = Self::get_game_info()?;

        #[cfg(target_os = "windows")]
        {
            if !yas::utils::is_admin() {
                return Err(anyhow!("请以管理员身份运行 / Please run as administrator"));
            }
        }

        let overrides = user_config.to_overrides();
        let mappings = Arc::new(MappingManager::new(&overrides)?);
        let mut ctrl = GenshinGameController::new(game_info)?;
        ctrl.focus_game_window();

        let ocr_backend = config.ocr_backend.as_deref().unwrap_or("ppocrv4");

        match config.debug_rescan_type.as_str() {
            "character" => {
                let mut char_config = Self::make_char_config(config, user_config);
                char_config.ocr_backend = ocr_backend.to_string();
                let debug_ocr = ocr_factory::create_ocr_model(&char_config.ocr_backend)?;
                let scanner = GoodCharacterScanner::new(char_config, mappings.clone())?;

                ctrl.key_press(enigo::Key::Layout('c'));
                yas::utils::sleep(1500);

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
                        info!("[rescan] 用户中断 / interrupted by user");
                        break;
                    }
                    println!("\n--- Re-scan iteration {} ---", i + 1);
                    let result = scanner.debug_scan_current(debug_ocr.as_ref(), &mut ctrl);
                    print_debug_result(&result);
                }

                ctrl.key_press(enigo::Key::Escape);
            }
            scan_type => {
                let tab = match scan_type {
                    "weapon" => "weapon",
                    "artifact" => "artifact",
                    _ => return Err(anyhow!("未知扫描类型 / Unknown rescan type: {}", scan_type)),
                };

                let scaler = {
                    let mut bp = BackpackScanner::new(&mut ctrl);
                    bp.open_backpack(1000);
                    bp.select_tab(tab, 500);
                    bp.scaler().clone()
                };

                if config.debug_start_at > 0 {
                    let items_per_page = GRID_COLS * GRID_ROWS;
                    let pages_to_skip = config.debug_start_at / items_per_page;
                    if pages_to_skip > 0 {
                        debug!("[rescan] scrolling {} pages ({} rows)...", pages_to_skip, pages_to_skip * GRID_ROWS);
                        let estimated_ticks = pages_to_skip * GRID_ROWS * 5;
                        for _ in 0..estimated_ticks {
                            ctrl.mouse_scroll(-1);
                        }
                        yas::utils::sleep(500);
                    }
                }

                let x = GRID_FIRST_X + col as f64 * GRID_OFFSET_X;
                let y = GRID_FIRST_Y + row as f64 * GRID_OFFSET_Y;

                let max_iter = if config.debug_rescan_count == 0 { usize::MAX } else { config.debug_rescan_count };

                match tab {
                    "weapon" => {
                        let mut weapon_config = Self::make_weapon_config(config, user_config);
                        weapon_config.ocr_backend = ocr_backend.to_string();
                        let debug_ocr = ocr_factory::create_ocr_model(&weapon_config.ocr_backend)?;
                        let scanner = GoodWeaponScanner::new(weapon_config, mappings.clone())?;

                        for i in 0..max_iter {
                            if yas::utils::is_rmb_down() {
                                info!("[rescan] 用户中断 / interrupted by user");
                                break;
                            }
                            println!("\n--- Re-scan iteration {} ---", i + 1);
                            ctrl.move_to(x, y);
                            yas::utils::sleep(50);
                            ctrl.click_at(x, y);
                            yas::utils::sleep(500);
                            let image = ctrl.capture_game()?;
                            let result = scanner.debug_scan_single(debug_ocr.as_ref(), &image, &scaler);
                            print_debug_result(&result);
                        }
                    }
                    "artifact" => {
                        let mut artifact_config = Self::make_artifact_config(config, user_config);
                        artifact_config.ocr_backend = ocr_backend.to_string();
                        let debug_ocr = ocr_factory::create_ocr_model(&artifact_config.ocr_backend)?;
                        let scanner = GoodArtifactScanner::new(artifact_config, mappings.clone())?;

                        for i in 0..max_iter {
                            if yas::utils::is_rmb_down() {
                                info!("[rescan] 用户中断 / interrupted by user");
                                break;
                            }
                            println!("\n--- Re-scan iteration {} ---", i + 1);
                            ctrl.move_to(x, y);
                            yas::utils::sleep(50);
                            ctrl.click_at(x, y);
                            yas::utils::sleep(500);
                            let image = ctrl.capture_game()?;
                            let result = scanner.debug_scan_single(debug_ocr.as_ref(), &image, &scaler);
                            print_debug_result(&result);
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }

        info!("=== 重扫完成 / Re-scan complete ===");
        Ok(())
    }

    /// Server mode: start the artifact manager HTTP server.
    fn run_server_mode(
        &self,
        config: &GoodScannerConfig,
        user_config: &GoodUserConfig,
    ) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            if !yas::utils::is_admin() {
                return Err(anyhow!("请以管理员身份运行 / Please run as administrator"));
            }
        }

        // Load mappings
        info!("=== 加载映射数据 / Loading mappings ===");
        let overrides = user_config.to_overrides();
        let mappings = Arc::new(MappingManager::new(&overrides)?);
        info!(
            "已加载 / Loaded: {} characters, {} weapons, {} artifact sets",
            mappings.character_name_map.len(),
            mappings.weapon_name_map.len(),
            mappings.artifact_set_map.len(),
        );

        // Find game window
        let game_info = Self::get_game_info()?;
        let mut ctrl = GenshinGameController::new(game_info)?;

        // Create artifact manager
        let ocr_backend = config.ocr_backend.clone().unwrap_or_else(|| "ppocrv5".to_string());
        let substat_ocr_backend = config.artifact_substat_ocr.clone();
        let mut manager = crate::manager::orchestrator::ArtifactManager::new(
            mappings,
            ocr_backend,
            substat_ocr_backend,
        );
        manager.delay_grid_item = user_config.artifact_grid_delay;
        manager.delay_scroll = user_config.artifact_scroll_delay;

        // Start HTTP server (blocks forever, always enabled in CLI mode)
        let enabled = Arc::new(std::sync::atomic::AtomicBool::new(true));
        crate::server::run_server(config.server_port, &mut ctrl, &manager, enabled)
    }

    /// Standalone diff mode: compare two existing JSON files without game.
    fn run_standalone_diff(compare_path: &str, actual_path: &str) -> Result<()> {
        info!("=== 离线对比模式 / Standalone diff mode ===");
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
                "对比发现 {} 个错误 / Diff found {} errors",
                result.summary.total_errors(), result.summary.total_errors()
            ));
        }
        info!("文件匹配 / Files match!");
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

    let secs_per_day = 86400u64;
    let secs_per_hour = 3600u64;
    let secs_per_min = 60u64;

    let days = now / secs_per_day;
    let remaining = now % secs_per_day;
    let hours = remaining / secs_per_hour;
    let remaining = remaining % secs_per_hour;
    let minutes = remaining / secs_per_min;
    let seconds = remaining % secs_per_min;

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

// ================================================================
// Core functions for GUI reuse
// ================================================================

/// Standalone scan configuration (no clap dependency).
#[derive(Clone, Debug)]
pub struct ScanCoreConfig {
    pub scan_characters: bool,
    pub scan_weapons: bool,
    pub scan_artifacts: bool,
    pub weapon_min_rarity: i32,
    pub artifact_min_rarity: i32,
    pub verbose: bool,
    pub continue_on_failure: bool,
    pub log_progress: bool,
    pub dump_images: bool,
    pub output_dir: String,
    pub ocr_backend: Option<String>,
    pub artifact_substat_ocr: String,
    pub char_max_count: usize,
    pub weapon_max_count: usize,
    pub artifact_max_count: usize,
    pub weapon_skip_delay: bool,
    pub artifact_skip_delay: bool,
}

impl Default for ScanCoreConfig {
    fn default() -> Self {
        Self {
            scan_characters: true,
            scan_weapons: true,
            scan_artifacts: true,
            weapon_min_rarity: 3,
            artifact_min_rarity: 4,
            verbose: false,
            continue_on_failure: false,
            log_progress: false,
            dump_images: false,
            output_dir: ".".to_string(),
            ocr_backend: None,
            artifact_substat_ocr: "ppocrv4".to_string(),
            char_max_count: 0,
            weapon_max_count: 0,
            artifact_max_count: 0,
            weapon_skip_delay: false,
            artifact_skip_delay: false,
        }
    }
}

impl ScanCoreConfig {
    /// Convert to the internal GoodScannerConfig fields needed by make_*_config.
    fn to_scanner_config(&self) -> GoodScannerConfig {
        GoodScannerConfig {
            scan_characters: self.scan_characters,
            scan_weapons: self.scan_weapons,
            scan_artifacts: self.scan_artifacts,
            scan_all: false,
            verbose: self.verbose,
            continue_on_failure: self.continue_on_failure,
            log_progress: self.log_progress,
            output_dir: self.output_dir.clone(),
            ocr_backend: self.ocr_backend.clone(),
            dump_images: self.dump_images,
            weapon_min_rarity: self.weapon_min_rarity,
            artifact_min_rarity: self.artifact_min_rarity,
            char_max_count: self.char_max_count,
            weapon_max_count: self.weapon_max_count,
            artifact_max_count: self.artifact_max_count,
            weapon_skip_delay: self.weapon_skip_delay,
            artifact_skip_delay: self.artifact_skip_delay,
            artifact_substat_ocr: self.artifact_substat_ocr.clone(),
            server_mode: false,
            server_port: 8765,
            debug_compare: None,
            debug_actual: None,
            debug_start_at: 0,
            debug_char_index: 0,
            debug_timing: false,
            debug_rescan_pos: None,
            debug_rescan_type: "weapon".to_string(),
            debug_rescan_count: 1,
        }
    }
}

/// Run a scan without CLI arg parsing. Returns the export path on success.
///
/// This is the core scan logic extracted from `GoodScannerApplication::run()`,
/// usable from both CLI and GUI.
pub fn run_scan_core(
    user_config: &GoodUserConfig,
    config: &ScanCoreConfig,
    status_fn: Option<&dyn Fn(&str)>,
) -> Result<String> {
    let report = |msg: &str| {
        if let Some(f) = status_fn { f(msg); }
    };

    #[cfg(target_os = "windows")]
    {
        if !yas::utils::is_admin() {
            return Err(anyhow!("请以管理员身份运行 / Please run as administrator"));
        }
    }

    let scanner_config = config.to_scanner_config();

    // Fetch and load mappings
    report("加载映射数据 / Loading mappings...");
    info!("=== 加载映射数据 / Loading mappings ===");
    let overrides = user_config.to_overrides();
    let mappings = Arc::new(MappingManager::new(&overrides)?);
    info!(
        "已加载 / Loaded: {} characters, {} weapons, {} artifact sets",
        mappings.character_name_map.len(),
        mappings.weapon_name_map.len(),
        mappings.artifact_set_map.len(),
    );

    // Find and focus the game window
    report("查找游戏窗口 / Finding game window...");
    let game_info = GoodScannerApplication::get_game_info()?;
    let mut ctrl = GenshinGameController::new(game_info)?;
    ctrl.focus_game_window();

    let mut characters = None;
    let mut weapons = None;
    let mut artifacts = None;

    // Scan characters
    if config.scan_characters {
        report("扫描角色 / Scanning characters...");
        info!("=== 扫描角色 / Scanning characters ===");
        let char_config = GoodScannerApplication::make_char_config(&scanner_config, user_config);
        let scanner = GoodCharacterScanner::new(char_config, mappings.clone())?;
        let result = scanner.scan(&mut ctrl, 0)?;
        info!("已扫描 / Scanned {} characters", result.len());
        characters = Some(result);

        if !yas::utils::was_aborted() {
            ctrl.return_to_main_ui(4);
        }
    }

    // Scan weapons
    if config.scan_weapons && !yas::utils::was_aborted() {
        report("扫描武器 / Scanning weapons...");
        info!("=== 扫描武器 / Scanning weapons ===");
        let weapon_config = GoodScannerApplication::make_weapon_config(&scanner_config, user_config);
        let scanner = GoodWeaponScanner::new(weapon_config, mappings.clone())?;
        let result = scanner.scan(&mut ctrl, false, 0)?;
        info!("已扫描 / Scanned {} weapons", result.len());
        weapons = Some(result);
    }

    // Scan artifacts
    if config.scan_artifacts && !yas::utils::was_aborted() {
        report("扫描圣遗物 / Scanning artifacts...");
        info!("=== 扫描圣遗物 / Scanning artifacts ===");
        let artifact_config = GoodScannerApplication::make_artifact_config(&scanner_config, user_config);
        let skip_open = config.scan_weapons;
        let scanner = GoodArtifactScanner::new(artifact_config, mappings.clone())?;
        let result = scanner.scan(&mut ctrl, skip_open, 0)?;
        info!("已扫描 / Scanned {} artifacts", result.len());
        artifacts = Some(result);
    }

    if yas::utils::was_aborted() {
        info!("扫描被用户中断 / Scan aborted by user (right-click)");
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
    let path_str = path.display().to_string();
    info!("已导出 / Exported to {}", path_str);

    Ok(path_str)
}

/// Run the artifact manager HTTP server (blocks until the server is stopped).
///
/// The `enabled` flag controls whether POST /manage requests are executed.
/// When false, the server still runs but returns 503 for manage requests.
/// Health and CORS endpoints always respond.
pub fn run_server_core(
    user_config: &GoodUserConfig,
    server_port: u16,
    ocr_backend: Option<&str>,
    artifact_substat_ocr: &str,
    enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        if !yas::utils::is_admin() {
            return Err(anyhow!("请以管理员身份运行 / Please run as administrator"));
        }
    }

    info!("=== 加载映射数据 / Loading mappings ===");
    let overrides = user_config.to_overrides();
    let mappings = Arc::new(MappingManager::new(&overrides)?);
    info!(
        "已加载 / Loaded: {} characters, {} weapons, {} artifact sets",
        mappings.character_name_map.len(),
        mappings.weapon_name_map.len(),
        mappings.artifact_set_map.len(),
    );

    let game_info = GoodScannerApplication::get_game_info()?;
    let mut ctrl = GenshinGameController::new(game_info)?;

    let ocr_be = ocr_backend.unwrap_or("ppocrv5").to_string();
    let mut manager = crate::manager::orchestrator::ArtifactManager::new(
        mappings,
        ocr_be,
        artifact_substat_ocr.to_string(),
    );
    manager.delay_grid_item = user_config.artifact_grid_delay;
    manager.delay_scroll = user_config.artifact_scroll_delay;

    crate::server::run_server(server_port, &mut ctrl, &manager, enabled)
}

/// Execute manage instructions from a JSON string.
pub fn run_manage_json(
    user_config: &GoodUserConfig,
    json_str: &str,
    ocr_backend: Option<&str>,
    artifact_substat_ocr: &str,
) -> Result<crate::manager::models::ManageResult> {
    #[cfg(target_os = "windows")]
    {
        if !yas::utils::is_admin() {
            return Err(anyhow!("请以管理员身份运行 / Please run as administrator"));
        }
    }

    let request: crate::manager::models::ArtifactManageRequest =
        serde_json::from_str(json_str)
            .map_err(|e| anyhow!("JSON解析失败 / JSON parse error: {}", e))?;

    info!(
        "执行 {} 条管理指令 / Executing {} manage instructions",
        request.instructions.len(), request.instructions.len()
    );

    let overrides = user_config.to_overrides();
    let mappings = Arc::new(MappingManager::new(&overrides)?);

    let game_info = GoodScannerApplication::get_game_info()?;
    let mut ctrl = GenshinGameController::new(game_info)?;

    let ocr_be = ocr_backend.unwrap_or("ppocrv5").to_string();
    let mut manager = crate::manager::orchestrator::ArtifactManager::new(
        mappings,
        ocr_be,
        artifact_substat_ocr.to_string(),
    );
    manager.delay_grid_item = user_config.artifact_grid_delay;
    manager.delay_scroll = user_config.artifact_scroll_delay;

    let result = manager.execute(&mut ctrl, request);
    Ok(result)
}
