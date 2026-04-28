use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::{command, ArgMatches, Args, FromArgMatches};
use yas::{log_debug, log_info, log_warn, log_error};
use serde::{Deserialize, Serialize};

use yas::game_info::{GameInfo, GameInfoBuilder};

use crate::scanner::artifact::GoodArtifactScannerConfig;
use crate::scanner::character::GoodCharacterScannerConfig;
use crate::scanner::common::constants::*;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::{MappingManager, NameOverrides};
use crate::scanner::common::models::GoodExport;
use crate::scanner::common::ocr_pool::{OcrPoolConfig, SharedOcrPools};
use crate::scanner::common::scan_runner::{
    run_scan_phases, ScanFailurePolicy, ScanRunOptions,
};
use crate::scanner::weapon::GoodWeaponScannerConfig;

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

/// Mirror URLs to try in order.
/// NuGet global CDN first (Akamai, good China connectivity), then GitHub proxies,
/// then GitHub direct. NuGet .nupkg is a ZIP with a different internal DLL path
/// (see `extract_onnxruntime_dll`).
#[cfg(target_os = "windows")]
const ORT_DOWNLOAD_URLS: &[&str] = &[
    "https://globalcdn.nuget.org/packages/microsoft.ml.onnxruntime.1.22.0.nupkg",
    "https://gh-proxy.com/https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-win-x64-1.22.0.zip",
    "https://ghfast.top/https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-win-x64-1.22.0.zip",
    "https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-win-x64-1.22.0.zip",
];

/// Minimum expected size for onnxruntime.dll (v1.22.0 x64 is ~12.6 MB).
/// Anything smaller is almost certainly a corrupted or partial download.
#[cfg(target_os = "windows")]
const ORT_DLL_MIN_SIZE: u64 = 5 * 1024 * 1024; // 5 MB

/// VC++ runtime DLLs required by onnxruntime.dll (from VS 2015-2022 Redistributable x64).
#[cfg(target_os = "windows")]
const VCPP_REQUIRED_DLLS: &[&str] = &[
    "VCRUNTIME140.dll",
    "VCRUNTIME140_1.dll",
    "MSVCP140.dll",
    "MSVCP140_1.dll",
];

/// Check if the Visual C++ 2015-2022 Redistributable (x64) is installed by
/// verifying that all required runtime DLLs can be loaded.
///
/// Returns `Ok(())` if all DLLs are present, or `Err` with a user-facing
/// message listing the missing DLLs and a download link.
#[cfg(target_os = "windows")]
pub fn check_vcpp_runtime() -> Result<()> {
    use windows_sys::Win32::System::LibraryLoader::LoadLibraryExW;
    use windows_sys::Win32::System::LibraryLoader::LOAD_LIBRARY_SEARCH_SYSTEM32;

    let mut missing = Vec::new();
    for &dll_name in VCPP_REQUIRED_DLLS {
        let wide: Vec<u16> = dll_name.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32)
        };
        if handle.is_null() {
            missing.push(dll_name);
        } else {
            unsafe { windows_sys::Win32::Foundation::FreeLibrary(handle) };
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    const VCPP_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";
    let missing_list = missing.join(", ");
    log_error!(
        "缺少 Visual C++ 运行库: {}",
        "Missing Visual C++ runtime DLLs: {}",
        missing_list
    );
    log_error!(
        "请安装 VC++ 2015-2022 Redistributable (x64): {}",
        "Install VC++ 2015-2022 Redistributable (x64): {}",
        VCPP_URL
    );
    Err(anyhow!(
        "缺少 Visual C++ 运行库 ({})，OCR引擎无法加载。\n\
         请下载安装: {}\n\
         / Missing Visual C++ runtime ({}). The OCR engine cannot load.\n\
         Please install from: {}",
        missing_list,
        VCPP_URL,
        missing_list,
        VCPP_URL,
    ))
}

/// Check if ONNX Runtime is available. Returns true if found, false if download needed.
/// Also detects corrupted/truncated files by checking the DLL size.
#[cfg(target_os = "windows")]
pub fn check_onnxruntime() -> bool {
    let dll_path = exe_dir().join(ORT_DLL_NAME);
    match std::fs::metadata(&dll_path) {
        Ok(meta) if meta.len() >= ORT_DLL_MIN_SIZE => {
            std::env::set_var("ORT_DYLIB_PATH", &dll_path);
            true
        }
        Ok(meta) => {
            log_warn!(
                "onnxruntime.dll 文件异常（{} 字节），将重新下载",
                "onnxruntime.dll looks corrupted ({} bytes), will re-download",
                meta.len()
            );
            // Remove the bad file so download_onnxruntime_inner can write fresh
            let _ = std::fs::remove_file(&dll_path);
            false
        }
        Err(_) => false, // File doesn't exist
    }
}

/// Download ONNX Runtime without interactive prompts (for GUI mode).
#[cfg(target_os = "windows")]
pub fn download_onnxruntime() -> Result<()> {
    let dll_path = exe_dir().join(ORT_DLL_NAME);
    log_info!("正在下载 ONNX Runtime...", "Downloading ONNX Runtime...");
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
        log_info!("尝试源 {}: {}", "Trying source {}:  {}", i + 1, url);

        match client.get(*url).send() {
            Ok(response) => {
                if !response.status().is_success() {
                    last_error = format!("HTTP {}", response.status());
                    log_warn!("源 {} 失败: {}", "Source {} failed: {}", i + 1, last_error);
                    continue;
                }
                match response.bytes() {
                    Ok(bytes) => {
                        log_info!("下载完成（{}字节），正在解压...", "Downloaded ({} bytes), extracting...", bytes.len());
                        match extract_onnxruntime_dll(&bytes, dll_path) {
                            Ok(()) => {
                                log_info!("ONNX Runtime 已安装到: {}", "installed to: {}", dll_path.display());
                                std::env::set_var("ORT_DYLIB_PATH", dll_path);
                                return Ok(());
                            }
                            Err(e) => {
                                last_error = format!("{}", e);
                                log_warn!("解压失败: {}", "Extract failed: {}", last_error);
                                if let Err(e) = std::fs::remove_file(dll_path) {
                                    log_warn!("清理失败的下载文件失败: {}", "Failed to clean up partial download: {}", e);
                                }
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        last_error = format!("{}", e);
                        log_warn!("下载失败: {}", "Download failed: {}", last_error);
                        continue;
                    }
                }
            }
            Err(e) => {
                last_error = format!("{}", e);
                log_warn!("连接失败: {}", "Connection failed: {}", last_error);
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
    check_vcpp_runtime()?;

    if check_onnxruntime() {
        return Ok(());
    }

    println!();
    println!("=======================================================");
    println!("  {} {}", yas::lang::localize("未找到 / Not found:"), ORT_DLL_NAME);
    println!("=======================================================");
    println!();
    println!("{}", yas::lang::localize("OCR引擎需要ONNX Runtime运行库。 / The OCR engine requires the ONNX Runtime library."));
    println!();
    println!("{}", yas::lang::localize("按回车自动下载（约70MB），或按 Ctrl+C 退出。 / Press Enter to download automatically (~70MB), or Ctrl+C to exit."));
    let _ = std::io::stdin().read_line(&mut String::new());

    download_onnxruntime()
}

/// Extract onnxruntime.dll from the downloaded zip archive.
///
/// Supports two layouts:
/// - GitHub release zip: `onnxruntime-win-x64-*/lib/onnxruntime.dll`
/// - NuGet .nupkg: `runtimes/win-x64/native/onnxruntime.dll`
#[cfg(target_os = "windows")]
fn extract_onnxruntime_dll(zip_bytes: &[u8], dest: &std::path::Path) -> Result<()> {
    use std::io::{Cursor, Read};
    let reader = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| anyhow!("无法打开压缩包 / Cannot open zip archive: {}", e))?;

    let suffixes = [
        "lib/onnxruntime.dll",               // GitHub release zip
        "runtimes/win-x64/native/onnxruntime.dll", // NuGet .nupkg
    ];

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)
            .map_err(|e| anyhow!("无法读取压缩包条目 / Cannot read zip entry: {}", e))?;
        let name = file.name().to_string();
        if suffixes.iter().any(|s| name.ends_with(s)) {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            std::fs::write(dest, &buf)?;
            return Ok(());
        }
    }

    Err(anyhow!("压缩包中未找到 onnxruntime.dll / onnxruntime.dll not found in zip archive"))
}

// ================================================================
// Rayon thread pool with larger stack
// ================================================================

/// Initialize the global rayon thread pool with an 8 MB per-thread stack.
///
/// ONNX Runtime's C++ inference code uses deep call stacks that can overflow
/// rayon's default 1 MB thread stack on Windows, triggering 0xc0000409
/// (STATUS_STACK_BUFFER_OVERRUN / __fastfail).  Call this once before any
/// scan or server work.  Harmless if called multiple times — the second call
/// is a no-op (rayon ignores `build_global` after the pool is already built).
pub fn init_rayon_pool() {
    let _ = rayon::ThreadPoolBuilder::new()
        .stack_size(8 * 1024 * 1024)
        .build_global();
}

// ================================================================
// User config (good_config.json)
// ================================================================

fn default_char_tab_delay() -> u64 { DEFAULT_DELAY_CHAR_TAB_SWITCH }
fn default_char_next_delay() -> u64 { DEFAULT_DELAY_CHAR_NEXT }
fn default_open_delay() -> u64 { DEFAULT_DELAY_OPEN_SCREEN }
fn default_close_delay() -> u64 { DEFAULT_DELAY_CLOSE_SCREEN }
fn default_scroll_delay() -> u64 { DEFAULT_DELAY_SCROLL }
fn default_tab_delay() -> u64 { DEFAULT_DELAY_INV_TAB_SWITCH }
fn default_weapon_panel_delay() -> u64 { DEFAULT_WEAPON_PANEL_DELAY }
fn default_artifact_initial_wait() -> u64 { DEFAULT_ARTIFACT_INITIAL_WAIT }
fn default_artifact_panel_timeout() -> u64 { DEFAULT_ARTIFACT_PANEL_TIMEOUT }
fn default_artifact_extra_delay() -> u64 { DEFAULT_ARTIFACT_EXTRA_DELAY }

fn default_mgr_transition() -> u64 { 1500 }
fn default_mgr_action() -> u64 { 800 }
fn default_mgr_cell() -> u64 { 100 }
fn default_mgr_scroll() -> u64 { 400 }

fn default_true() -> bool { true }
fn default_server_port() -> u16 { 8765 }

/// Deserialize a u64 that may arrive as a number, a numeric string, or an
/// empty/invalid string.  Non-numeric values silently fall back to 0 so that
/// `#[serde(default = "…")]` can supply the real default.
fn deserialize_u64_lenient<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct U64LenientVisitor;

    impl<'de> de::Visitor<'de> for U64LenientVisitor {
        type Value = u64;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a u64 or a numeric string")
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<u64, E> { Ok(v) }
        fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<u64, E> { Ok(v.max(0) as u64) }
        fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<u64, E> { Ok(v.max(0.0) as u64) }

        fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<u64, E> {
            Ok(v.trim().parse::<u64>().unwrap_or(0))
        }

        fn visit_none<E: de::Error>(self) -> std::result::Result<u64, E> { Ok(0) }
        fn visit_unit<E: de::Error>(self) -> std::result::Result<u64, E> { Ok(0) }
    }

    deserializer.deserialize_any(U64LenientVisitor)
}

fn is_default_char_tab_delay(v: &u64) -> bool { *v == DEFAULT_DELAY_CHAR_TAB_SWITCH }
fn is_default_char_next_delay(v: &u64) -> bool { *v == DEFAULT_DELAY_CHAR_NEXT }
fn is_default_open_delay(v: &u64) -> bool { *v == DEFAULT_DELAY_OPEN_SCREEN }
fn is_default_close_delay(v: &u64) -> bool { *v == DEFAULT_DELAY_CLOSE_SCREEN }
fn is_default_scroll_delay(v: &u64) -> bool { *v == DEFAULT_DELAY_SCROLL }
fn is_default_tab_delay(v: &u64) -> bool { *v == DEFAULT_DELAY_INV_TAB_SWITCH }
fn is_default_weapon_panel_delay(v: &u64) -> bool { *v == DEFAULT_WEAPON_PANEL_DELAY }
fn is_default_artifact_initial_wait(v: &u64) -> bool { *v == DEFAULT_ARTIFACT_INITIAL_WAIT }
fn is_default_artifact_panel_timeout(v: &u64) -> bool { *v == DEFAULT_ARTIFACT_PANEL_TIMEOUT }
fn is_default_artifact_extra_delay(v: &u64) -> bool { *v == DEFAULT_ARTIFACT_EXTRA_DELAY }

/// Fields in GoodUserConfig that must be unsigned integers.
/// If the JSON has an invalid value (e.g. empty string from old config versions),
/// we remove the field so serde fills in its default.
const U64_FIELDS: &[&str] = &[
    "char_tab_delay", "char_next_delay", "char_open_delay", "char_close_delay",
    "inv_scroll_delay", "inv_tab_delay", "inv_open_delay",
    "weapon_panel_delay", "artifact_initial_wait", "artifact_panel_timeout", "artifact_extra_delay",
    "mgr_transition_delay", "mgr_action_delay", "mgr_cell_delay", "mgr_scroll_delay",
    // Old aliases — also sanitize in case they appear
    "weapon_scroll_delay", "artifact_scroll_delay", "weapon_tab_delay", "artifact_tab_delay",
    "weapon_open_delay", "artifact_open_delay",
];

/// Sanitize a parsed JSON object: remove u64 fields that have non-numeric values
/// (e.g. empty strings from old config migrations) so serde defaults apply.
fn sanitize_config_json(val: &mut serde_json::Value) {
    let obj = match val.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    for &field in U64_FIELDS {
        let should_remove = match obj.get(field) {
            Some(serde_json::Value::Number(_)) => false,
            Some(_) => true, // string, null, bool, etc. — not a valid u64
            None => false,
        };
        if should_remove {
            obj.remove(field);
        }
    }
}

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
    // Only serialized when user set a value different from default.

    #[serde(default = "default_char_tab_delay", skip_serializing_if = "is_default_char_tab_delay", deserialize_with = "deserialize_u64_lenient")]
    pub char_tab_delay: u64,
    #[serde(default = "default_char_next_delay", skip_serializing_if = "is_default_char_next_delay", deserialize_with = "deserialize_u64_lenient")]
    pub char_next_delay: u64,
    #[serde(default = "default_open_delay", skip_serializing_if = "is_default_open_delay", deserialize_with = "deserialize_u64_lenient")]
    pub char_open_delay: u64,
    #[serde(default = "default_close_delay", skip_serializing_if = "is_default_close_delay", deserialize_with = "deserialize_u64_lenient")]
    pub char_close_delay: u64,

    #[serde(default = "default_scroll_delay", skip_serializing_if = "is_default_scroll_delay", deserialize_with = "deserialize_u64_lenient", alias = "weapon_scroll_delay", alias = "artifact_scroll_delay")]
    pub inv_scroll_delay: u64,
    #[serde(default = "default_tab_delay", skip_serializing_if = "is_default_tab_delay", deserialize_with = "deserialize_u64_lenient", alias = "weapon_tab_delay", alias = "artifact_tab_delay")]
    pub inv_tab_delay: u64,
    #[serde(default = "default_open_delay", skip_serializing_if = "is_default_open_delay", deserialize_with = "deserialize_u64_lenient", alias = "weapon_open_delay", alias = "artifact_open_delay")]
    pub inv_open_delay: u64,

    /// Weapon: fixed delay (ms) before panel stability check.
    #[serde(default = "default_weapon_panel_delay", skip_serializing_if = "is_default_weapon_panel_delay", deserialize_with = "deserialize_u64_lenient")]
    pub weapon_panel_delay: u64,

    /// Artifact: initial wait (ms) after click before starting panel detection.
    #[serde(default = "default_artifact_initial_wait", skip_serializing_if = "is_default_artifact_initial_wait", deserialize_with = "deserialize_u64_lenient")]
    pub artifact_initial_wait: u64,

    /// Artifact: timeout (ms) for fingerprint-based panel load detection.
    #[serde(default = "default_artifact_panel_timeout", skip_serializing_if = "is_default_artifact_panel_timeout", deserialize_with = "deserialize_u64_lenient")]
    pub artifact_panel_timeout: u64,

    /// Artifact: extra delay (ms) after panel load, before capture.
    #[serde(default = "default_artifact_extra_delay", skip_serializing_if = "is_default_artifact_extra_delay", deserialize_with = "deserialize_u64_lenient")]
    pub artifact_extra_delay: u64,

    // --- Manager delay settings ---
    /// Screen transition delay for the manager (ms). Default: 1500.
    #[serde(default = "default_mgr_transition", deserialize_with = "deserialize_u64_lenient")]
    pub mgr_transition_delay: u64,
    /// Action button delay for the manager (ms). Default: 800.
    #[serde(default = "default_mgr_action", deserialize_with = "deserialize_u64_lenient")]
    pub mgr_action_delay: u64,
    /// Grid cell click delay for the manager (ms). Default: 100.
    #[serde(default = "default_mgr_cell", deserialize_with = "deserialize_u64_lenient")]
    pub mgr_cell_delay: u64,
    /// Scroll settle delay for the manager (ms). Default: 400.
    #[serde(default = "default_mgr_scroll", deserialize_with = "deserialize_u64_lenient")]
    pub mgr_scroll_delay: u64,

    /// GUI language preference: "zh" or "en".
    #[serde(default)]
    pub lang: String,

    // --- GUI advanced settings (persisted so they survive restarts) ---

    #[serde(default = "default_true")]
    pub scan_characters: bool,
    #[serde(default = "default_true")]
    pub scan_weapons: bool,
    #[serde(default = "default_true")]
    pub scan_artifacts: bool,
    #[serde(default)]
    pub verbose: bool,
    #[serde(default)]
    pub continue_on_failure: bool,
    #[serde(default)]
    pub dump_images: bool,
    #[serde(default)]
    pub hdr_mode: bool,
    #[serde(default)]
    pub dump_job_data: bool,
    #[serde(default)]
    pub save_on_cancel: bool,
    #[serde(default)]
    pub char_max_count: usize,
    #[serde(default)]
    pub weapon_max_count: usize,
    #[serde(default)]
    pub artifact_max_count: usize,
    #[serde(default = "default_server_port")]
    pub server_port: u16,
    #[serde(default = "default_true")]
    pub update_inventory: bool,

    /// Advanced: force OCR v5 pool size. 0 = auto-detect from RAM. Non-zero forces that size.
    #[serde(default)]
    pub ocr_pool_v5_override: usize,
    /// Advanced: force OCR v4 pool size. 0 = auto-detect from RAM. Non-zero forces that size.
    #[serde(default)]
    pub ocr_pool_v4_override: usize,
}

impl GoodUserConfig {
    fn opt(s: &str) -> Option<String> {
        if s.trim().is_empty() { None } else { Some(s.trim().to_string()) }
    }

    pub fn to_overrides(&self) -> NameOverrides {
        NameOverrides {
            traveler_name: Self::opt(&self.traveler_name),
            wanderer_name: Self::opt(&self.wanderer_name),
            manekin_name: Self::opt(&self.manekin_name),
            manekina_name: Self::opt(&self.manekina_name),
        }
    }

    /// Resolve OCR pool sizes: auto-detect from RAM, then apply any non-zero user overrides.
    pub fn resolve_ocr_pool_config(&self) -> OcrPoolConfig {
        let mut cfg = OcrPoolConfig::detect();
        if self.ocr_pool_v5_override > 0 {
            log_info!(
                "OCR v5 池大小手动覆盖: {} → {}",
                "OCR v5 pool size manually overridden: {} → {}",
                cfg.v5_count, self.ocr_pool_v5_override,
            );
            cfg.v5_count = self.ocr_pool_v5_override;
        }
        if self.ocr_pool_v4_override > 0 {
            log_info!(
                "OCR v4 池大小手动覆盖: {} → {}",
                "OCR v4 pool size manually overridden: {} → {}",
                cfg.v4_count, self.ocr_pool_v4_override,
            );
            cfg.v4_count = self.ocr_pool_v4_override;
        }
        cfg
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
            char_next_delay: default_char_next_delay(),
            char_open_delay: default_open_delay(),
            char_close_delay: default_close_delay(),
            inv_scroll_delay: default_scroll_delay(),
            inv_tab_delay: default_tab_delay(),
            inv_open_delay: default_open_delay(),
            weapon_panel_delay: default_weapon_panel_delay(),
            artifact_initial_wait: default_artifact_initial_wait(),
            artifact_panel_timeout: default_artifact_panel_timeout(),
            artifact_extra_delay: default_artifact_extra_delay(),
            mgr_transition_delay: default_mgr_transition(),
            mgr_action_delay: default_mgr_action(),
            mgr_cell_delay: default_mgr_cell(),
            mgr_scroll_delay: default_mgr_scroll(),
            lang: String::new(),
            scan_characters: true,
            scan_weapons: true,
            scan_artifacts: true,
            verbose: false,
            continue_on_failure: false,
            dump_images: false,
            hdr_mode: false,
            dump_job_data: false,
            save_on_cancel: false,
            char_max_count: 0,
            weapon_max_count: 0,
            artifact_max_count: 0,
            server_port: default_server_port(),
            update_inventory: true,
            ocr_pool_v5_override: 0,
            ocr_pool_v4_override: 0,
        }
    }
}

/// Load the user config from data/good_config.json without interactive prompts.
/// Returns defaults if the file does not exist or cannot be parsed.
pub fn load_config_or_default() -> GoodUserConfig {
    let path = config_path();
    if !path.exists() {
        return GoodUserConfig::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            // Parse as generic JSON first so we can sanitize invalid field types
            // (e.g. empty strings in u64 fields from old config versions).
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(&contents);
            let config_result = match parsed {
                Ok(mut val) => {
                    sanitize_config_json(&mut val);
                    serde_json::from_value::<GoodUserConfig>(val)
                }
                Err(e) => Err(e),
            };
            match config_result {
                Ok(config) => {
                    config
                }
                Err(e) => {
                    log_error!("配置文件解析失败，将使用默认值: {}: {}", "Config parse error (using defaults): {}: {}", path.display(), e);
                    GoodUserConfig::default()
                }
            }
        }
        Err(e) => {
            log_error!("配置文件读取失败，将使用默认值: {}: {}", "Config read error (using defaults): {}: {}", path.display(), e);
            GoodUserConfig::default()
        }
    }
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
/// If the file does not exist, return an error instructing the user to create it
/// (via the GUI, or manually).
fn load_or_create_config() -> Result<GoodUserConfig> {
    let path = config_path();
    log_debug!("正在查找配置文件: {}", "Looking for config at: {}", path.display());

    if !path.exists() {
        return Err(anyhow!(
            "配置文件不存在 / Config file not found: {}\n\
             请先运行 GUI（不带参数启动）创建配置文件，或手动创建。\n\
             Please run the GUI first (launch without arguments) to create the config file,\n\
             or create it manually at the path above.",
            path.display()
        ));
    }

    let contents = std::fs::read_to_string(&path)?;
    // Parse as generic JSON first so we can sanitize invalid field types
    // (e.g. empty strings in u64 fields from old config versions).
    let mut val: serde_json::Value = serde_json::from_str(&contents)
        .map_err(|e| anyhow!("配置解析失败 / Failed to parse {}: {}", path.display(), e))?;
    sanitize_config_json(&mut val);
    let config: GoodUserConfig = serde_json::from_value(val)
        .map_err(|e| anyhow!("配置解析失败 / Failed to parse {}: {}", path.display(), e))?;
    log_debug!("已加载配置: {}", "Loaded config from {}", path.display());

    // Re-save to strip invalid/default entries and add any new default fields
    if let Err(e) = save_config(&config) {
        log_warn!("配置重新保存失败: {}", "Config re-save failed: {}", e);
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

    /// HDR显示模式 / HDR display mode
    #[arg(long = "hdr-mode", help = "使用HDR像素阈值（默认使用SDR阈值）\nUse HDR pixel thresholds (default uses SDR thresholds)",
          help_heading = "通用选项 / Global Options")]
    pub hdr_mode: bool,

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

    // weapon_skip_delay and artifact_skip_delay removed — grid-based detection always used

    /// 圣遗物副词条OCR后端 / Artifact substat OCR backend
    #[arg(long = "artifact-substat-ocr", help = "圣遗物副词条OCR后端\nArtifact substat/general OCR backend",
          default_value = "ppocrv4", help_heading = "扫描器配置 / Scanner Config")]
    pub artifact_substat_ocr: String,
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
            next_delay: user_config.char_next_delay,
            open_delay: user_config.char_open_delay,
            close_delay: user_config.char_close_delay,
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
            delay_scroll: user_config.inv_scroll_delay,
            delay_tab: user_config.inv_tab_delay,
            open_delay: user_config.inv_open_delay,
            continue_on_failure: config.continue_on_failure,
            log_progress: config.log_progress,
            dump_images: config.dump_images,
            max_count: config.weapon_max_count,
            panel_delay: user_config.weapon_panel_delay,
        }
    }

    /// Build an artifact scanner config from global CLI flags + JSON config.
    pub fn make_artifact_config(config: &GoodScannerConfig, user_config: &GoodUserConfig) -> GoodArtifactScannerConfig {
        GoodArtifactScannerConfig {
            min_rarity: config.artifact_min_rarity,
            verbose: config.verbose,
            ocr_backend: config.ocr_backend.clone().unwrap_or_else(|| "ppocrv5".to_string()),
            substat_ocr_backend: config.artifact_substat_ocr.clone(),
            delay_scroll: user_config.inv_scroll_delay,
            delay_tab: user_config.inv_tab_delay,
            open_delay: user_config.inv_open_delay,
            continue_on_failure: config.continue_on_failure,
            log_progress: config.log_progress,
            dump_images: config.dump_images,
            max_count: config.artifact_max_count,
            initial_wait: user_config.artifact_initial_wait,
            panel_timeout: user_config.artifact_panel_timeout,
            extra_delay: user_config.artifact_extra_delay,
        }
    }

    pub fn run(&self) -> Result<()> {
        println!("{}", yas::lang::localize("正在启动扫描器... / GOOD Scanner starting..."));

        init_rayon_pool();

        // Check for ONNX Runtime before doing anything else
        #[cfg(target_os = "windows")]
        ensure_onnxruntime()?;

        let arg_matches = &self.arg_matches;
        let config = GoodScannerConfig::from_arg_matches(arg_matches)?;

        // === Load user config (good_config.json) ===
        let user_config = load_or_create_config()?;

        // Determine what to scan (default: all if no flags specified)
        let no_flags = !config.scan_characters && !config.scan_weapons && !config.scan_artifacts && !config.scan_all;

        let scan_config = ScanCoreConfig {
            scan_characters: config.scan_characters || config.scan_all || no_flags,
            scan_weapons: config.scan_weapons || config.scan_all || no_flags,
            scan_artifacts: config.scan_artifacts || config.scan_all || no_flags,
            weapon_min_rarity: config.weapon_min_rarity,
            artifact_min_rarity: config.artifact_min_rarity,
            verbose: config.verbose,
            continue_on_failure: config.continue_on_failure,
            log_progress: config.log_progress,
            dump_images: config.dump_images,
            hdr_mode: config.hdr_mode || user_config.hdr_mode,
            output_dir: config.output_dir.clone(),
            ocr_backend: config.ocr_backend.clone(),
            artifact_substat_ocr: config.artifact_substat_ocr.clone(),
            char_max_count: config.char_max_count,
            weapon_max_count: config.weapon_max_count,
            artifact_max_count: config.artifact_max_count,
            save_on_cancel: false,
        };

        run_scan_core(&user_config, &scan_config, None, None)?;
        Ok(())
    }

}

/// Generate a local-time timestamp string like "2024-01-15_12-30-45".
#[cfg(target_os = "windows")]
pub fn chrono_timestamp() -> String {
    #[repr(C)]
    struct SystemTime {
        year: u16, month: u16, _dow: u16, day: u16,
        hour: u16, minute: u16, second: u16, _ms: u16,
    }
    extern "system" {
        fn GetLocalTime(lpSystemTime: *mut SystemTime);
    }
    let mut st = SystemTime { year: 0, month: 0, _dow: 0, day: 0, hour: 0, minute: 0, second: 0, _ms: 0 };
    unsafe { GetLocalTime(&mut st) };
    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        st.year, st.month, st.day, st.hour, st.minute, st.second
    )
}

#[cfg(not(target_os = "windows"))]
pub fn chrono_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let secs_per_day = 86400u64;
    let days = now / secs_per_day;
    let remaining = now % secs_per_day;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;
    let mut y = 1970i32;
    let mut d = days as i32;
    loop {
        let dy = if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 366 } else { 365 };
        if d < dy { break; }
        d -= dy;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let md: &[i32] = if leap {
        &[31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        &[31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 1;
    for &days_in_month in md {
        if d < days_in_month { break; }
        d -= days_in_month;
        m += 1;
    }
    format!("{:04}-{:02}-{:02}_{:02}-{:02}-{:02}", y, m, d + 1, hours, minutes, seconds)
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
    pub hdr_mode: bool,
    pub output_dir: String,
    pub ocr_backend: Option<String>,
    pub artifact_substat_ocr: String,
    pub char_max_count: usize,
    pub weapon_max_count: usize,
    pub artifact_max_count: usize,
    /// If true, export partial results when the user cancels mid-scan.
    pub save_on_cancel: bool,
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
            hdr_mode: false,
            output_dir: ".".to_string(),
            ocr_backend: None,
            artifact_substat_ocr: "ppocrv4".to_string(),
            char_max_count: 0,
            weapon_max_count: 0,
            artifact_max_count: 0,
            save_on_cancel: false,
        }
    }
}

impl ScanCoreConfig {
    /// Convert to the internal GoodScannerConfig fields needed by make_*_config.
    pub fn to_scanner_config(&self) -> GoodScannerConfig {
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
            hdr_mode: self.hdr_mode,
            weapon_min_rarity: self.weapon_min_rarity,
            artifact_min_rarity: self.artifact_min_rarity,
            char_max_count: self.char_max_count,
            weapon_max_count: self.weapon_max_count,
            artifact_max_count: self.artifact_max_count,
            artifact_substat_ocr: self.artifact_substat_ocr.clone(),
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
    cancel_token: Option<yas::cancel::CancelToken>,
) -> Result<String> {
    init_rayon_pool();
    crate::scanner::common::annotator::init(config.dump_images);
    crate::scanner::common::pixel_profile::set_hdr_mode(config.hdr_mode);

    let report = |msg: &str| {
        if let Some(f) = status_fn { f(msg); }
    };

    #[cfg(target_os = "windows")]
    {
        yas::utils::ensure_admin()?;
    }

    // Fetch and load mappings
    report("加载映射数据 / Loading mappings...");
    log_info!("加载映射数据...", "Loading mappings...");
    let overrides = user_config.to_overrides();
    let mappings = Arc::new(MappingManager::new(&overrides)?);
    log_info!(
        "已加载: {} 角色, {} 武器, {} 圣遗物套装",
        "Loaded: {} characters, {} weapons, {} artifact sets", mappings.character_name_map.len(), mappings.weapon_name_map.len(), mappings.artifact_set_map.len());

    // Find and focus the game window
    report("查找游戏窗口 / Finding game window...");
    let game_info = GoodScannerApplication::get_game_info()?;
    log_info!(
        "游戏窗口: {}x{}",
        "Game window: {}x{}",
        game_info.window.width, game_info.window.height,
    );

    report("初始化屏幕截图 / Initializing screen capture...");
    let mut ctrl = GenshinGameController::new(game_info)
        .context("屏幕截图初始化失败 / Screen capture initialization failed")?;
    let token = cancel_token.unwrap_or_else(yas::cancel::CancelToken::new);

    // Create shared OCR pools for all scanners
    report("加载OCR模型 / Loading OCR models...");
    let pool_config = user_config.resolve_ocr_pool_config();
    let ocr_backend = config.ocr_backend.as_deref().unwrap_or("ppocrv5");
    let substat_backend = config.artifact_substat_ocr.as_str();
    let pools = Arc::new(SharedOcrPools::new(pool_config, ocr_backend, substat_backend)?);
    log_info!("OCR模型加载完成", "OCR models loaded");

    let save_on_cancel = config.save_on_cancel;
    let scan_result = run_scan_phases(
        &mut ctrl,
        mappings,
        pools,
        user_config,
        config,
        None,
        status_fn,
        token.clone(),
        ScanRunOptions {
            save_on_cancel,
            accept_cancelled_success: true,
            failure_policy: ScanFailurePolicy::StopOnError,
        },
    )?;

    let characters = scan_result.characters.into_complete();
    let weapons = scan_result.weapons.into_complete();
    let artifacts = scan_result.artifacts.into_complete();

    if token.is_cancelled() {
        log_info!("扫描被用户中断", "Scan stopped by user");
        if !save_on_cancel {
            return Err(anyhow!("扫描被用户中断 / Scan stopped by user"));
        }
    }

    // Wait for any pending debug image writes to finish
    crate::scanner::common::annotator::flush();

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
    log_info!("已导出: {}", "Exported to {}", path_str);

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
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop_on_all_matched: bool,
    dump_images: bool,
    dump_job_data: bool,
) -> Result<()> {
    init_rayon_pool();
    crate::scanner::common::annotator::init(dump_images);
    crate::scanner::common::pixel_profile::set_hdr_mode(user_config.hdr_mode);

    #[cfg(target_os = "windows")]
    {
        yas::utils::ensure_admin()?;
    }

    log_info!("加载映射数据...", "Loading mappings...");
    let overrides = user_config.to_overrides();
    let mappings = Arc::new(MappingManager::new(&overrides)?);
    log_info!(
        "已加载: {}角色, {}武器, {}套装",
        "Loaded: {} characters, {} weapons, {} artifact sets", mappings.character_name_map.len(), mappings.weapon_name_map.len(), mappings.artifact_set_map.len());

    let ocr_be = ocr_backend.unwrap_or("ppocrv5").to_string();
    let substat_ocr = artifact_substat_ocr.to_string();
    let scroll_delay = user_config.inv_scroll_delay;
    let capture_delay = user_config.artifact_extra_delay;
    let panel_timeout = user_config.artifact_panel_timeout;
    let initial_wait = user_config.artifact_initial_wait;
    let mappings_clone = mappings.clone();

    let mgr_delays = crate::manager::ui_actions::ManagerDelays {
        transition: user_config.mgr_transition_delay,
        action: user_config.mgr_action_delay,
        cell: user_config.mgr_cell_delay,
        scroll: user_config.mgr_scroll_delay,
    };

    let scan_defaults = ScanCoreConfig {
        scan_characters: true,
        scan_weapons: true,
        scan_artifacts: true,
        dump_images,
        hdr_mode: user_config.hdr_mode,
        ocr_backend: ocr_backend.map(|s| s.to_string()),
        artifact_substat_ocr: artifact_substat_ocr.to_string(),
        ..ScanCoreConfig::default()
    };
    let exec_user_config = user_config.clone();

    // Fn closure so the server can retry init on subsequent jobs if the first
    // attempt fails (e.g. game window not open yet). All captures are cloned
    // inside the body so each call builds fresh state.
    let init_executor = move || -> anyhow::Result<Box<dyn crate::server::ManageExecutor>> {

        crate::manager::ui_actions::set_manager_delays(mgr_delays.clone());
        log_info!("查找游戏窗口...", "Finding game window...");
        let game_info = GoodScannerApplication::get_game_info()?;
        log_info!("初始化屏幕截图...", "Initializing screen capture...");
        let ctrl = GenshinGameController::new(game_info)?;
        log_info!("加载OCR模型...", "Loading OCR models...");
        let pool_config = exec_user_config.resolve_ocr_pool_config();
        let pools = Arc::new(SharedOcrPools::new(pool_config, &ocr_be, &substat_ocr)
            .context("OCR模型加载失败，请确认内存充足（建议8GB以上）\
                     / OCR model load failed — ensure sufficient memory (8 GB+ recommended)")?);
        let manager = crate::manager::orchestrator::ArtifactManager::new(
            mappings_clone.clone(),
            pools,
            capture_delay,
            scroll_delay,
            panel_timeout,
            initial_wait,
            stop_on_all_matched,
            dump_images,
        );
        Ok(Box::new(crate::server::GameExecutor {
            ctrl,
            manager,
            user_config: exec_user_config.clone(),
            scan_defaults: scan_defaults.clone(),
        }))
    };

    crate::server::run_server(server_port, init_executor, enabled, shutdown, dump_job_data)
}

/// Execute manage instructions from a JSON string.
pub fn run_manage_json(
    user_config: &GoodUserConfig,
    json_str: &str,
    ocr_backend: Option<&str>,
    artifact_substat_ocr: &str,
    cancel_token: Option<yas::cancel::CancelToken>,
) -> Result<crate::manager::models::ManageResult> {
    crate::scanner::common::pixel_profile::set_hdr_mode(user_config.hdr_mode);
    #[cfg(target_os = "windows")]
    {
        yas::utils::ensure_admin()?;
    }

    crate::manager::ui_actions::set_manager_delays(crate::manager::ui_actions::ManagerDelays {
        transition: user_config.mgr_transition_delay,
        action: user_config.mgr_action_delay,
        cell: user_config.mgr_cell_delay,
        scroll: user_config.mgr_scroll_delay,
    });

    let request: crate::manager::models::LockManageRequest =
        serde_json::from_str(json_str)
            .map_err(|e| anyhow!("JSON解析失败 / JSON parse error: {}", e))?;

    let total = request.lock.len() + request.unlock.len();
    log_info!(
        "执行 {} 条管理请求（lock: {}, unlock: {}）",
        "Executing {} manage items (lock: {}, unlock: {})", total, request.lock.len(), request.unlock.len());

    let overrides = user_config.to_overrides();
    let mappings = Arc::new(MappingManager::new(&overrides)?);

    let game_info = GoodScannerApplication::get_game_info()?;
    let mut ctrl = GenshinGameController::new(game_info)?;
    let token = cancel_token.unwrap_or_else(yas::cancel::CancelToken::new);

    let ocr_be = ocr_backend.unwrap_or("ppocrv5");
    let pool_config = user_config.resolve_ocr_pool_config();
    let pools = Arc::new(SharedOcrPools::new(pool_config, ocr_be, artifact_substat_ocr)?);
    let manager = crate::manager::orchestrator::ArtifactManager::new(
        mappings,
        pools,
        user_config.artifact_extra_delay,
        user_config.inv_scroll_delay,
        user_config.artifact_panel_timeout,
        user_config.artifact_initial_wait,
        false,
        false, // dump_images: offline JSON mode doesn't support it
    );

    let (result, _artifact_snapshot) = manager.execute(&mut ctrl, request, None, token);
    Ok(result)
}
