use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Result};
use log::{debug, info};
use serde::Deserialize;

const MAPPINGS_URL: &str = "https://ggartifact.com/good/mappings.json";
const MAPPINGS_CACHE_PATH: &str = "data/mappings.json";
const MAPPINGS_META_PATH: &str = "data/mappings_meta.json";
const MAPPINGS_TTL_SECS: u64 = 24 * 3600; // 1 day

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

#[derive(Deserialize)]
struct MappingsMeta {
    #[serde(rename = "lastFetchTime")]
    last_fetch_time: u64,
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

impl MappingManager {
    /// Fetch mappings if needed (cache expired or missing), then load and initialize.
    ///
    /// Port of `fetchMappingsIfNeeded()` + `initMappings()` from GOODScanner
    pub fn new(overrides: &NameOverrides) -> Result<Self> {
        Self::fetch_if_needed()?;
        Self::load_from_cache(overrides)
    }

    /// Check cache freshness and fetch from remote if needed.
    fn fetch_if_needed() -> Result<()> {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Check last fetch time from meta file
        let mut last_fetch_time: u64 = 0;
        if let Ok(meta_raw) = std::fs::read_to_string(MAPPINGS_META_PATH) {
            if let Ok(meta) = serde_json::from_str::<MappingsMeta>(&meta_raw) {
                last_fetch_time = meta.last_fetch_time;
            }
        }

        let cache_exists = Path::new(MAPPINGS_CACHE_PATH).exists();

        // Skip fetch if cache is fresh
        if cache_exists && (now_secs - last_fetch_time) < MAPPINGS_TTL_SECS {
            return Ok(());
        }

        info!("正在获取游戏数据映射... / Fetching game data mappings...");

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
                    let meta = format!(
                        "{{\"lastFetchTime\":{}}}",
                        now_secs
                    );
                    std::fs::write(MAPPINGS_META_PATH, meta)?;
                    debug!("游戏数据映射已更新 / Game data mappings updated");
                } else {
                    if cache_exists {
                        info!(
                            "获取数据失败（HTTP {}），使用本地缓存 / Fetch failed (HTTP {}), using local cache",
                            response.status(), response.status()
                        );
                    } else {
                        bail!(
                            "获取游戏数据失败 / Failed to fetch game data: HTTP {}",
                            response.status()
                        );
                    }
                }
            }
            Err(e) => {
                if cache_exists {
                    info!(
                        "获取数据失败（{}），使用本地缓存 / Fetch failed ({}), using local cache",
                        e, e
                    );
                } else {
                    bail!(
                        "获取游戏数据失败且无本地缓存 / Failed to fetch game data (no local cache): {}",
                        e
                    );
                }
            }
        }

        Ok(())
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
