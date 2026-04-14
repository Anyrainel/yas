use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use yas::{log_debug, log_info, log_warn};
use serde::{Deserialize, Serialize};

const MAPPINGS_URL: &str = "https://ggartifact.com/good/mappings.json";
const MAPPINGS_CACHE_PATH: &str = "data/mappings.json";
const MAPPINGS_META_PATH: &str = "data/mappings_meta.json";
const MAPPINGS_TTL_SECS: u64 = 24 * 3600; // 1 day

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct MappingsMeta {
    #[serde(rename = "lastFetchTime")]
    last_fetch_time: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn load_meta() -> MappingsMeta {
    if let Ok(content) = fs::read_to_string(MAPPINGS_META_PATH) {
        if let Ok(meta) = serde_json::from_str::<MappingsMeta>(&content) {
            return meta;
        }
    }
    MappingsMeta::default()
}

fn save_meta(meta: &MappingsMeta) {
    if let Some(parent) = Path::new(MAPPINGS_META_PATH).parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            log_warn!("无法创建缓存目录: {}", "Cannot create cache directory: {}", e);
        }
    }
    match serde_json::to_string(meta) {
        Ok(json) => {
            if let Err(e) = fs::write(MAPPINGS_META_PATH, json) {
                log_warn!("无法保存映射缓存信息: {}", "Cannot save mapping cache metadata: {}", e);
            }
        }
        Err(e) => {
            log_warn!("无法序列化缓存信息: {}", "Cannot serialize cache metadata: {}", e);
        }
    }
}

/// Delete cached files and re-download immediately.
pub fn force_refresh() -> Result<()> {
    let _ = fs::remove_file(MAPPINGS_META_PATH);
    let _ = fs::remove_file(MAPPINGS_CACHE_PATH);
    fetch_if_needed()
}

fn is_fresh(last_fetch_time: u64, ttl_secs: u64) -> bool {
    last_fetch_time > 0 && (now_secs() - last_fetch_time) < ttl_secs
}

/// Constellation bonus info for a character
#[derive(Debug, Clone)]
pub struct ConstBonus {
    /// Which talent gets +3 at C3: "A" (auto), "E" (skill), or "Q" (burst)
    pub c3: Option<String>,
    /// Which talent gets +3 at C5: "A" (auto), "E" (skill), or "Q" (burst)
    pub c5: Option<String>,
}

/// Holds all name→GOOD key mappings loaded from remote/cached data.
///
/// Port of the mapping system from GOODScanner/lib/constants.js and
/// GOODScanner/lib/fetch_mappings.js
#[derive(Debug)]
pub struct MappingManager {
    /// Chinese character name → GOOD character key
    pub character_name_map: HashMap<String, String>,
    /// GOOD character key → constellation talent bonus info
    pub character_const_bonus: HashMap<String, ConstBonus>,
    /// Chinese weapon name → GOOD weapon key
    pub weapon_name_map: HashMap<String, String>,
    /// Chinese artifact set name → GOOD set key
    pub artifact_set_map: HashMap<String, String>,
    /// GOOD set key → max rarity (4 or 5)
    pub artifact_set_max_rarity: HashMap<String, i32>,
}

// --- JSON deserialization types for the remote mappings.json ---

#[derive(Deserialize)]
struct MappingsFile {
    characters: Vec<CharacterEntry>,
    weapons: Vec<WeaponEntry>,
    #[serde(rename = "artifactSets")]
    artifact_sets: Vec<ArtifactSetEntry>,
}

#[derive(Deserialize)]
struct CharacterEntry {
    id: String,
    #[serde(alias = "names")]
    n: LocalizedNames,
    c3: Option<String>,
    c5: Option<String>,
}

#[derive(Deserialize)]
struct WeaponEntry {
    id: String,
    #[serde(alias = "names")]
    n: LocalizedNames,
}

#[derive(Deserialize)]
struct ArtifactSetEntry {
    id: String,
    #[serde(alias = "names")]
    n: LocalizedNames,
    #[serde(alias = "rarity")]
    r: Option<i32>,
}

#[derive(Deserialize)]
struct LocalizedNames {
    zh: Option<String>,
}

/// Name override config for characters with customizable in-game names
pub struct NameOverrides {
    pub traveler_name: Option<String>,
    pub wanderer_name: Option<String>,
    pub manekin_name: Option<String>,
    pub manekina_name: Option<String>,
}

impl Default for NameOverrides {
    fn default() -> Self {
        Self {
            traveler_name: None,
            wanderer_name: None,
            manekin_name: None,
            manekina_name: None,
        }
    }
}

/// Check cache freshness and fetch from remote if needed.
fn fetch_if_needed() -> Result<()> {
    let meta = load_meta();
    let cache_exists = Path::new(MAPPINGS_CACHE_PATH).exists();

    // Skip fetch if cache is fresh
    if cache_exists && is_fresh(meta.last_fetch_time, MAPPINGS_TTL_SECS) {
        return Ok(());
    }

    log_info!("正在获取游戏数据映射...", "Fetching game data mappings...");

    // Ensure data directory exists
    if let Some(parent) = Path::new(MAPPINGS_CACHE_PATH).parent() {
        std::fs::create_dir_all(parent)?;
    }

    match reqwest::blocking::get(MAPPINGS_URL) {
        Ok(response) => {
            if response.status().is_success() {
                let body = response.text()?;
                // Validate JSON
                let _: serde_json::Value = serde_json::from_str(&body)?;
                std::fs::write(MAPPINGS_CACHE_PATH, &body)?;
                save_meta(&MappingsMeta {
                    last_fetch_time: now_secs(),
                });
                log_debug!("游戏数据映射已更新", "Game data mappings updated");
            } else {
                if cache_exists {
                    log_warn!(
                        "获取数据失败 (HTTP {})，使用本地缓存",
                        "Fetch failed (HTTP {}), using local cache",
                        response.status()
                    );
                } else {
                    bail!(
                        "获取游戏数据失败 (HTTP {})，且无本地缓存。请检查网络连接。\n\
                         / Failed to fetch game data (HTTP {}), no local cache. Check your network connection.",
                        response.status(), response.status()
                    );
                }
            }
        }
        Err(e) => {
            if cache_exists {
                log_warn!(
                    "获取数据失败 ({})，使用本地缓存",
                    "Fetch failed ({}), using local cache",
                    e
                );
            } else {
                bail!(
                    "获取游戏数据失败且无本地缓存。请检查网络连接，或手动下载 {} 到 data/ 目录。\n\
                     / Failed to fetch game data (no local cache). Check your network connection, \
                     or manually download {} to the data/ folder.\n\
                     错误 / Error: {}",
                    MAPPINGS_URL, MAPPINGS_URL, e
                );
            }
        }
    }

    Ok(())
}

impl MappingManager {
    /// Fetch mappings if needed (cache expired or missing), then load and initialize.
    ///
    /// Port of `fetchMappingsIfNeeded()` + `initMappings()` from GOODScanner
    pub fn new(overrides: &NameOverrides) -> Result<Self> {
        fetch_if_needed()?;
        Self::load_from_cache(overrides)
    }

    /// Load mappings from the local cache file.
    fn load_from_cache(overrides: &NameOverrides) -> Result<Self> {
        let raw = std::fs::read_to_string(MAPPINGS_CACHE_PATH)?;
        let data: MappingsFile = serde_json::from_str(&raw)?;

        let mut character_name_map = HashMap::new();
        let mut character_const_bonus = HashMap::new();

        for entry in &data.characters {
            if let Some(zh_name) = &entry.n.zh {
                character_name_map.insert(zh_name.clone(), entry.id.clone());
            }
            if entry.c3.is_some() || entry.c5.is_some() {
                character_const_bonus.insert(
                    entry.id.clone(),
                    ConstBonus {
                        c3: entry.c3.clone(),
                        c5: entry.c5.clone(),
                    },
                );
            }
        }

        let mut weapon_name_map = HashMap::new();
        for entry in &data.weapons {
            if let Some(zh_name) = &entry.n.zh {
                weapon_name_map.insert(zh_name.clone(), entry.id.clone());
            }
        }

        let mut artifact_set_map = HashMap::new();
        let mut artifact_set_max_rarity = HashMap::new();
        for entry in &data.artifact_sets {
            if let Some(zh_name) = &entry.n.zh {
                artifact_set_map.insert(zh_name.clone(), entry.id.clone());
            }
            if let Some(rarity) = entry.r {
                artifact_set_max_rarity.insert(entry.id.clone(), rarity);
            }
        }

        // Apply user name overrides
        let name_overrides: &[(&Option<String>, &str)] = &[
            (&overrides.traveler_name, "Traveler"),
            (&overrides.wanderer_name, "Wanderer"),
            (&overrides.manekin_name, "Manekin"),
            (&overrides.manekina_name, "Manekina"),
        ];

        for (custom_name, id) in name_overrides {
            if let Some(name) = custom_name {
                let trimmed = name.trim();
                if !trimmed.is_empty() {
                    character_name_map.insert(trimmed.to_string(), id.to_string());
                }
            }
        }

        Ok(Self {
            character_name_map,
            character_const_bonus,
            weapon_name_map,
            artifact_set_map,
            artifact_set_max_rarity,
        })
    }
}
