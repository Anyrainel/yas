/// Shared metadata for all cached data files (`data/metadata.json`).
///
/// Previously each cache had its own meta file (`mappings_meta.json`,
/// `data_cache_meta.json`).  This module unifies them into one file and
/// migrates the old files on first load.
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const META_PATH: &str = "data/metadata.json";
const OLD_MAPPINGS_META: &str = "data/mappings_meta.json";
const OLD_DATA_CACHE_META: &str = "data/data_cache_meta.json";

/// Timestamps tracking when each remote resource was last fetched.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CacheMeta {
    #[serde(default, rename = "mappingsLastFetchTime")]
    pub mappings_last_fetch_time: u64,
    #[serde(default, rename = "dataCacheLastFetchTime")]
    pub data_cache_last_fetch_time: u64,
}

/// In-process lock so concurrent callers don't race on the same file.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// Old per-file meta format (both used the same shape).
#[derive(Deserialize)]
struct OldMeta {
    #[serde(rename = "lastFetchTime")]
    last_fetch_time: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Load the unified metadata, migrating old files if necessary.
pub fn load() -> CacheMeta {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    if let Ok(content) = fs::read_to_string(META_PATH) {
        if let Ok(meta) = serde_json::from_str::<CacheMeta>(&content) {
            return meta;
        }
    }

    // Migrate from old per-file meta files
    let mut meta = CacheMeta::default();

    if let Ok(content) = fs::read_to_string(OLD_MAPPINGS_META) {
        if let Ok(old) = serde_json::from_str::<OldMeta>(&content) {
            meta.mappings_last_fetch_time = old.last_fetch_time;
        }
        let _ = fs::remove_file(OLD_MAPPINGS_META);
    }

    if let Ok(content) = fs::read_to_string(OLD_DATA_CACHE_META) {
        if let Ok(old) = serde_json::from_str::<OldMeta>(&content) {
            meta.data_cache_last_fetch_time = old.last_fetch_time;
        }
        let _ = fs::remove_file(OLD_DATA_CACHE_META);
    }

    // Persist the migrated (or empty) metadata
    let _ = save_inner(&meta);
    meta
}

/// Save the unified metadata to disk.
pub fn save(meta: &CacheMeta) {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = save_inner(meta);
}

fn save_inner(meta: &CacheMeta) -> std::io::Result<()> {
    fs::create_dir_all(Path::new(META_PATH).parent().unwrap())?;
    let content = serde_json::to_string(meta).unwrap();
    fs::write(META_PATH, content)
}

/// Check whether a cache is fresh (within TTL seconds of now).
pub fn is_fresh(last_fetch_time: u64, ttl_secs: u64) -> bool {
    last_fetch_time > 0 && (now_secs() - last_fetch_time) < ttl_secs
}

/// Return the current unix timestamp (seconds).
pub fn now() -> u64 {
    now_secs()
}
