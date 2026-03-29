//! Auto-update: check GitHub for new releases and self-replace the executable.
//!
//! Version scheme: CalVer tags (e.g. `v2026.03.27`) mapped to semver
//! `YYYYMMDD.0.0` in Cargo.toml by CI.  Dev builds (major < 20000000)
//! skip the update check entirely.
//!
//! **Version check** uses two strategies:
//!   1. GitHub REST API (`api.github.com`) — fast, structured JSON
//!   2. Redirect fallback (`/releases/latest` → 302 Location header) —
//!      works through download mirrors when the API is blocked
//!
//! **Downloads** use the same mirror chain as the ONNX Runtime download:
//!   ghfast.top → gh-proxy.com → direct GitHub

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Result};
use log::{debug, info};
use serde::Deserialize;

/// Asset filename to look for in each release.
const ASSET_NAME: &str = "GOODScanner.exe";

/// Download mirror prefixes, tried in order.  Empty string = direct GitHub.
const DOWNLOAD_MIRRORS: &[&str] = &[
    "https://ghfast.top/",
    "https://gh-proxy.com/",
    "", // direct GitHub
];

/// Base URL for the releases/latest redirect (non-API).
const RELEASES_LATEST_URL: &str =
    "https://github.com/Anyrainel/GOODScanner/releases/latest";

/// Minimum plausible exe size (1 MB).  The real binary is 20+ MB;
/// anything smaller is almost certainly an error page or truncated download.
const MIN_EXE_SIZE: usize = 1_000_000;

// ── GitHub API types (minimal) ───────────────────────────────────

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

// ── Public types ─────────────────────────────────────────────────

/// Result of an update check.
pub enum UpdateStatus {
    /// Already on the latest (or newer) version.
    UpToDate,
    /// A newer release exists on GitHub.
    UpdateAvailable {
        current_version: String,
        latest_version: String,
        /// Direct github.com download URL for the exe asset.
        download_url: String,
    },
    /// Running a dev build — skip update checks.
    DevBuild,
}

// ── Version helpers ──────────────────────────────────────────────

/// Parse a CalVer tag like `"v2026.03.27"` → `20260327u32`.
fn parse_calver_tag(tag: &str) -> Option<u32> {
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    let parts: Vec<&str> = tag.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let year: u32 = parts[0].parse().ok()?;
    let month: u32 = parts[1].parse().ok()?;
    let day: u32 = parts[2].parse().ok()?;
    if year < 2020 || month < 1 || month > 12 || day < 1 || day > 31 {
        return None;
    }
    Some(year * 10000 + month * 100 + day)
}

/// The current build's CalVer integer, or `None` for dev builds.
///
/// CI sets `CARGO_PKG_VERSION` to `YYYYMMDD.0.0`; the major component
/// is ≥ 20000000.  Local dev builds have `0.x.y` (major < 20000000).
fn current_version_int() -> Option<u32> {
    let version = env!("CARGO_PKG_VERSION");
    let major: u32 = version.split('.').next()?.parse().ok()?;
    if major < 20000000 {
        return None; // dev build
    }
    Some(major)
}

/// Human-readable current version string.
pub fn current_version_display() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let major: u32 = version
        .split('.')
        .next()
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    if major >= 20000000 {
        let year = major / 10000;
        let month = (major % 10000) / 100;
        let day = major % 100;
        format!("v{}.{:02}.{:02}", year, month, day)
    } else {
        format!("v{}", version)
    }
}

// ── Tag resolution strategies ────────────────────────────────────

/// Strategy 1: GitHub REST API (fast, works in most regions).
fn get_tag_via_api() -> Option<String> {
    let url = "https://api.github.com/repos/Anyrainel/GOODScanner/releases/latest";
    debug!("检查更新(API) / Checking via API: {}", url);

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .user_agent("GOODScanner-Updater")
        .build()
        .ok()?;

    let resp = client.get(url).send().ok()?;
    if !resp.status().is_success() {
        debug!("API 返回 / API returned: {}", resp.status());
        return None;
    }
    let release: GitHubRelease = resp.json().ok()?;
    // Validate it looks like a CalVer tag before returning
    if parse_calver_tag(&release.tag_name).is_some() {
        Some(release.tag_name)
    } else {
        debug!(
            "无法解析版本号 / Cannot parse release tag: {}",
            release.tag_name
        );
        None
    }
}

/// Strategy 2: Follow `/releases/latest` redirect through download mirrors.
///
/// GitHub responds with 302 → `/releases/tag/vYYYY.MM.DD`.
/// We disable redirect-following in reqwest and read the `Location` header.
/// Tried through each mirror prefix so it works when github.com is blocked.
fn get_tag_via_redirect() -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("GOODScanner-Updater")
        .build()
        .ok()?;

    for mirror in DOWNLOAD_MIRRORS {
        let url = if mirror.is_empty() {
            RELEASES_LATEST_URL.to_string()
        } else {
            format!("{}{}", mirror, RELEASES_LATEST_URL)
        };
        debug!("检查更新(redirect) / Checking via redirect: {}", url);

        let resp = match client.get(&url).send() {
            Ok(r) => r,
            Err(e) => {
                debug!("连接失败 / Connection failed: {}", e);
                continue;
            }
        };

        // Look for 3xx redirect with Location header
        if !resp.status().is_redirection() {
            debug!("非重定向响应 / Non-redirect response: {}", resp.status());
            continue;
        }

        let location = match resp.headers().get("location") {
            Some(v) => match v.to_str() {
                Ok(s) => s.to_string(),
                Err(_) => continue,
            },
            None => continue,
        };

        if let Some(tag) = extract_tag_from_url(&location) {
            return Some(tag.to_string());
        }
    }

    None
}

/// Extract a CalVer tag from a URL containing `/releases/tag/vX.Y.Z`.
fn extract_tag_from_url(url: &str) -> Option<&str> {
    let marker = "/releases/tag/";
    let rest = url.split(marker).nth(1)?;
    // Tag ends at next '/', '?', '#', or end-of-string
    let tag = rest.split(&['/', '?', '#'][..]).next()?;
    // Validate it parses as CalVer
    if parse_calver_tag(tag).is_some() {
        Some(tag)
    } else {
        None
    }
}

// ── Public: update check ─────────────────────────────────────────

/// Query GitHub for the latest release and compare with the running version.
///
/// Tries the REST API first (structured, fast), then falls back to the
/// redirect-based approach through download mirrors (works in China when
/// `api.github.com` is blocked).
pub fn check_for_update() -> Result<UpdateStatus> {
    let current_int = match current_version_int() {
        Some(v) => v,
        None => return Ok(UpdateStatus::DevBuild),
    };

    // Try API first, then redirect fallback
    let latest_tag = get_tag_via_api()
        .or_else(|| {
            debug!("API失败，尝试redirect方式 / API failed, trying redirect");
            get_tag_via_redirect()
        })
        .ok_or_else(|| anyhow!("无法获取最新版本信息 / Cannot determine latest version"))?;

    let latest_int = parse_calver_tag(&latest_tag).ok_or_else(|| {
        anyhow!(
            "无法解析版本号 / Cannot parse release tag: {}",
            latest_tag
        )
    })?;

    if latest_int <= current_int {
        return Ok(UpdateStatus::UpToDate);
    }

    // Construct download URL from tag (don't rely on API assets list)
    let download_url = format!(
        "https://github.com/Anyrainel/GOODScanner/releases/download/{}/{}",
        latest_tag, ASSET_NAME,
    );

    Ok(UpdateStatus::UpdateAvailable {
        current_version: current_version_display(),
        latest_version: latest_tag,
        download_url,
    })
}

// ── Public: cleanup ──────────────────────────────────────────────

/// Delete the leftover `.old` executable from a previous update.
/// Safe to call unconditionally at every startup.
pub fn cleanup_old_exe() {
    if let Ok(exe) = std::env::current_exe() {
        let old = exe.with_extension("exe.old");
        if old.exists() {
            match std::fs::remove_file(&old) {
                Ok(()) => debug!("已清理旧版本 / Cleaned up old exe"),
                Err(e) => debug!("清理旧版本失败 / Failed to clean up old exe: {}", e),
            }
        }
    }
}

// ── Public: download & self-replace ──────────────────────────────

/// Download the new release and replace the running executable.
///
/// On Windows the running exe cannot be overwritten, but it *can* be
/// renamed.  The sequence is:
///
/// 1. Rename `GOODScanner.exe` → `GOODScanner.exe.old`
/// 2. Write the downloaded bytes as `GOODScanner.exe`
/// 3. On next launch, `cleanup_old_exe()` removes the `.old` file
///
/// If writing the new file fails, the rename is rolled back so the
/// original exe is restored.
pub fn download_and_replace(download_url: &str) -> Result<PathBuf> {
    let exe_path = std::env::current_exe()
        .map_err(|e| anyhow!("无法获取当前程序路径 / Cannot get current exe path: {}", e))?;
    let old_path = exe_path.with_extension("exe.old");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .connect_timeout(Duration::from_secs(15))
        .user_agent("GOODScanner-Updater")
        .build()?;

    let mut last_error = String::new();

    for (i, mirror) in DOWNLOAD_MIRRORS.iter().enumerate() {
        let url = if mirror.is_empty() {
            download_url.to_string()
        } else {
            format!("{}{}", mirror, download_url)
        };

        info!(
            "尝试下载源 {}/{} / Trying source {}/{}: {}",
            i + 1,
            DOWNLOAD_MIRRORS.len(),
            i + 1,
            DOWNLOAD_MIRRORS.len(),
            url,
        );

        match client.get(&url).send() {
            Ok(resp) if resp.status().is_success() => match resp.bytes() {
                Ok(bytes) => {
                    info!(
                        "下载完成（{} 字节）/ Downloaded ({} bytes)",
                        bytes.len(),
                        bytes.len(),
                    );

                    // Must be a PE executable of plausible size
                    if bytes.get(..2) != Some(b"MZ") {
                        last_error =
                            "下载文件不是有效的exe / Not a valid PE executable".into();
                        info!("{}", last_error);
                        continue;
                    }
                    if bytes.len() < MIN_EXE_SIZE {
                        last_error = format!(
                            "文件过小（{} 字节 < {} 字节）/ File too small ({} < {} bytes)",
                            bytes.len(),
                            MIN_EXE_SIZE,
                            bytes.len(),
                            MIN_EXE_SIZE,
                        );
                        info!("{}", last_error);
                        continue;
                    }

                    // Self-replace: rename running exe → .old, write new
                    if old_path.exists() {
                        let _ = std::fs::remove_file(&old_path);
                    }
                    std::fs::rename(&exe_path, &old_path).map_err(|e| {
                        anyhow!("无法重命名当前程序 / Cannot rename current exe: {}", e)
                    })?;

                    if let Err(e) = std::fs::write(&exe_path, &bytes) {
                        // Rollback: restore the original exe
                        let _ = std::fs::rename(&old_path, &exe_path);
                        return Err(anyhow!(
                            "无法写入新版本 / Cannot write new version: {}",
                            e
                        ));
                    }

                    info!("更新完成！请重启程序。/ Update complete! Please restart.");
                    return Ok(exe_path);
                }
                Err(e) => {
                    last_error = format!("{}", e);
                    info!("下载失败 / Download failed: {}", last_error);
                }
            },
            Ok(resp) => {
                last_error = format!("HTTP {}", resp.status());
                info!(
                    "源 {} 失败 / Source {} failed: {}",
                    i + 1,
                    i + 1,
                    last_error,
                );
            }
            Err(e) => {
                last_error = format!("{}", e);
                info!("连接失败 / Connection failed: {}", last_error);
            }
        }
    }

    Err(anyhow!(
        "所有下载源均失败 / All download sources failed: {}",
        last_error
    ))
}
