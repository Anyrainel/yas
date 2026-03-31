/// Downloads and caches `data_cache.json` from ggartifact.com.
///
/// Uses the shared `data/metadata.json` for cache freshness (see `cache_meta`).
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use log::{info, warn};

use super::data_types::DataCache;
use crate::cache_meta;

const DATA_CACHE_URL: &str = "https://ggartifact.com/good/data_cache.json";
const DATA_CACHE_PATH: &str = "data/data_cache.json";
const DATA_CACHE_TTL_SECS: u64 = 24 * 3600;

/// Fetch `data_cache.json` from remote if cache is stale, otherwise load from cache.
/// Returns the parsed `DataCache`.
pub fn load_data_cache() -> Result<DataCache> {
    fs::create_dir_all("data").ok();

    let cache_path = Path::new(DATA_CACHE_PATH);
    let meta = cache_meta::load();

    if !cache_path.exists()
        || !cache_meta::is_fresh(meta.data_cache_last_fetch_time, DATA_CACHE_TTL_SECS)
    {
        info!("正在下载抓包数据缓存... / Downloading capture data cache...");
        match fetch_remote() {
            Ok(data) => {
                // Validate JSON before writing
                let _: DataCache = serde_json::from_str(&data)
                    .context("Failed to parse fetched data_cache.json")?;
                fs::write(cache_path, &data)?;
                let mut meta = cache_meta::load();
                meta.data_cache_last_fetch_time = cache_meta::now();
                cache_meta::save(&meta);
                info!("抓包数据缓存已更新 / Capture data cache updated");
            }
            Err(e) => {
                if cache_path.exists() {
                    warn!(
                        "下载抓包数据缓存失败（{}），使用本地缓存 / Failed to fetch data cache ({}), using stale cache",
                        e, e
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
