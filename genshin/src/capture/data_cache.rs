/// Downloads and caches `data_cache.json` from ggartifact.com.
///
/// Uses `data/data_cache_meta.json` for cache freshness tracking.
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use yas::{log_info, log_warn};

use super::data_types::DataCache;

const DATA_CACHE_URL: &str = "https://ggartifact.com/good/data_cache.json";
const DATA_CACHE_PATH: &str = "data/data_cache.json";
const DATA_CACHE_META_PATH: &str = "data/data_cache_meta.json";
const DATA_CACHE_TTL_SECS: u64 = 24 * 3600;

/// Unified metadata file written by the old cache_meta module.
/// Used for one-time migration only.
const UNIFIED_META_PATH: &str = "data/metadata.json";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct CacheMeta {
    #[serde(rename = "lastFetchTime")]
    last_fetch_time: u64,
}

/// Shape of the old unified metadata.json (read-only, for migration).
#[derive(Deserialize)]
struct UnifiedMeta {
    #[serde(default, rename = "dataCacheLastFetchTime")]
    data_cache_last_fetch_time: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn load_meta() -> CacheMeta {
    // Try reading the per-file meta
    if let Ok(content) = fs::read_to_string(DATA_CACHE_META_PATH) {
        if let Ok(meta) = serde_json::from_str::<CacheMeta>(&content) {
            return meta;
        }
    }

    // Migrate from unified metadata.json if it exists
    if let Ok(content) = fs::read_to_string(UNIFIED_META_PATH) {
        if let Ok(unified) = serde_json::from_str::<UnifiedMeta>(&content) {
            if unified.data_cache_last_fetch_time > 0 {
                let meta = CacheMeta {
                    last_fetch_time: unified.data_cache_last_fetch_time,
                };
                write_meta(&meta);
                return meta;
            }
        }
    }

    CacheMeta::default()
}

fn write_meta(meta: &CacheMeta) {
    if let Some(parent) = Path::new(DATA_CACHE_META_PATH).parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(meta) {
        let _ = fs::write(DATA_CACHE_META_PATH, json);
    }
}

fn is_cache_fresh(last_fetch_time: u64, ttl_secs: u64) -> bool {
    last_fetch_time > 0 && (now_secs() - last_fetch_time) < ttl_secs
}

/// Fetch `data_cache.json` from remote if cache is stale, otherwise load from cache.
/// Returns the parsed `DataCache`.
pub fn load_data_cache() -> Result<DataCache> {
    fs::create_dir_all("data").ok();

    let cache_path = Path::new(DATA_CACHE_PATH);
    let meta = load_meta();

    if !cache_path.exists()
        || !is_cache_fresh(meta.last_fetch_time, DATA_CACHE_TTL_SECS)
    {
        log_info!("正在下载抓包数据缓存...", "Downloading capture data cache...");
        match fetch_remote() {
            Ok(data) => {
                // Validate JSON before writing
                let _: DataCache = serde_json::from_str(&data)
                    .context("Failed to parse fetched data_cache.json")?;
                fs::write(cache_path, &data)?;
                write_meta(&CacheMeta {
                    last_fetch_time: now_secs(),
                });
                log_info!("抓包数据缓存已更新", "Capture data cache updated");
            }
            Err(e) => {
                if cache_path.exists() {
                    log_warn!(
                        "下载抓包数据缓存失败（{}），使用本地缓存",
                        "Failed to fetch data cache ({}), using stale cache",
                        e
                    );
                } else {
                    anyhow::bail!(
                        "下载抓包数据缓存失败且无本地缓存 / Failed to fetch data cache and no local cache exists: {}",
                        e
                    );
                }
            }
        }
    }

    let content = fs::read_to_string(cache_path).context("Failed to read data_cache.json")?;
    let data_cache: DataCache =
        serde_json::from_str(&content).context("Failed to parse data_cache.json")?;
    Ok(data_cache)
}

fn fetch_remote() -> Result<String> {
    let resp = reqwest::blocking::get(DATA_CACHE_URL)
        .context("HTTP request to ggartifact.com failed")?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {} from {}", status, DATA_CACHE_URL);
    }
    resp.text().context("Failed to read response body")
}
