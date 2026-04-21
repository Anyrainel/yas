/// Converts captured packet data into GOOD v3 export format.
///
/// Ported from irminsul's `player_data.rs`, adapted to output yas's own GOOD models.
use std::collections::HashMap;

use auto_artifactarium::r#gen::protos::{AvatarInfo, Item};
use indexmap::IndexMap;

use crate::scanner::common::models::{
    GoodArtifact, GoodCharacter, GoodExport, GoodSubStat, GoodTalent, GoodWeapon,
};

use super::data_types::{DataCache, Property, SkillType, to_good_key};

// --- Game-internal property IDs (content IDs, stable within a version) ---
// These come from the game's `AvatarInfo.prop_map` and may shift across major versions.
const PROP_LEVEL: u32 = 4001;
const PROP_ASCENSION: u32 = 1002;
// `avatar_type == 1` means a formal (playable) character, filtering out trial/test avatars.
const AVATAR_TYPE_FORMAL: u32 = 1;

/// Settings for filtering exported data.
///
/// Rarity rules are hardcoded (not user-configurable):
/// - Weapons: 3★ and above
/// - Artifacts: 4★ and above, only at their set's max rarity
///   (e.g. 4★ drops of a 5★ set are excluded)
#[derive(Clone, Debug)]
pub struct CaptureExportSettings {
    pub include_characters: bool,
    pub include_artifacts: bool,
    pub include_weapons: bool,

    pub min_character_level: u32,
    pub min_character_ascension: u32,
    pub min_character_constellation: u32,

    pub min_artifact_level: u32,

    pub min_weapon_level: u32,
    pub min_weapon_refinement: u32,
    pub min_weapon_ascension: u32,
}

impl Default for CaptureExportSettings {
    fn default() -> Self {
        Self {
            include_characters: true,
            include_artifacts: true,
            include_weapons: true,
            min_character_level: 0,
            min_character_ascension: 0,
            min_character_constellation: 0,
            min_artifact_level: 0,
            min_weapon_level: 0,
            min_weapon_refinement: 0,
            min_weapon_ascension: 0,
        }
    }
}

/// Minimum weapon rarity to export (3★+).
const MIN_WEAPON_RARITY: u32 = 3;
/// Minimum artifact rarity to export (4★+).
const MIN_ARTIFACT_RARITY: u32 = 4;

/// Artifact sets whose max rarity is 4★ (sourced from mappings.json).
/// The data_cache incorrectly lists some of these at rarity 5, so we hardcode
/// the true max rarity to avoid filtering out valid 4★ artifacts.
const MAX_4STAR_ARTIFACT_SETS: &[&str] = &[
    "Berserker",
    "BraveHeart",
    "DefendersWill",
    "Gambler",
    "Instructor",
    "MartialArtist",
    "PrayersForDestiny",
    "PrayersForIllumination",
    "PrayersForWisdom",
    "PrayersToSpringtime",
    "PrayersToTheFirmament",
    "ResolutionOfSojourner",
    "Scholar",
    "TheExile",
    "TinyMiracle",
];

/// Accumulates packet data and exports to GOOD format.
pub struct PlayerData {
    data_cache: DataCache,
    characters: Vec<AvatarInfo>,
    items: Vec<Item>,
    character_equip_guid_map: HashMap<u64, u32>,
}

impl PlayerData {
    pub fn new(data_cache: DataCache) -> Self {
        Self {
            data_cache,
            characters: Vec::new(),
            items: Vec::new(),
            character_equip_guid_map: HashMap::new(),
        }
    }

    /// Count characters (formal avatars only, avatar_type == 1).
    pub fn character_count(&self) -> usize {
        self.characters.iter().filter(|c| c.avatar_type == AVATAR_TYPE_FORMAL).count()
    }

    /// Count weapons among captured items.
    pub fn weapon_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.has_equip() && item.equip().has_weapon())
            .count()
    }

    /// Count artifacts (reliquaries) among captured items.
    pub fn artifact_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.has_equip() && item.equip().has_reliquary())
            .count()
    }

    pub fn process_characters(&mut self, avatars: &[AvatarInfo]) {
        self.character_equip_guid_map.clear();
        for avatar in avatars {
            for guid in &avatar.equip_guid_list {
                self.character_equip_guid_map
                    .insert(*guid, avatar.avatar_id);
            }
        }
        self.characters = avatars.into();
    }

    pub fn process_items(&mut self, items: &[Item]) {
        self.items = items.into();
    }

    pub fn export(&self, settings: &CaptureExportSettings) -> anyhow::Result<GoodExport> {
        let characters = if settings.include_characters {
            Some(self.export_characters(settings))
        } else {
            None
        };

        let weapons = if settings.include_weapons {
            Some(self.export_weapons(settings))
        } else {
            None
        };

        let artifacts = if settings.include_artifacts {
            Some(self.export_artifacts(settings))
        } else {
            None
        };

        Ok(GoodExport {
            format: "GOOD".to_string(),
            version: 3,
            source: "yas-GOODScanner".to_string(),
            characters,
            weapons,
            artifacts,
        })
    }

    fn resolve_location(&self, guid: u64) -> String {
        self.character_equip_guid_map
            .get(&guid)
            .and_then(|id| {
                self.data_cache
                    .get_character(*id)
                    .map(|name| to_good_key(name))
            })
            .unwrap_or_default()
    }

    fn export_characters(&self, settings: &CaptureExportSettings) -> Vec<GoodCharacter> {
        self.characters
            .iter()
            .filter_map(|character| {
                // Filter to formal avatars only (type 1)
                if character.avatar_type != AVATAR_TYPE_FORMAL {
                    return None;
                }

                let name = self.data_cache.get_character(character.avatar_id)?;
                let level = character.prop_map.get(&PROP_LEVEL).map(|prop| prop.val as u32)?;
                let ascension = character.prop_map.get(&PROP_ASCENSION).map(|prop| prop.val as u32)?;
                let constellation = character.talent_id_list.len() as u32;

                let mut auto = 1u32;
                let mut skill = 1u32;
                let mut burst = 1u32;

                for (id, lvl) in &character.skill_level_map {
                    let Some(ty) = self.data_cache.get_skill_type(*id) else {
                        continue;
                    };
                    match ty {
                        SkillType::Auto => auto = *lvl,
                        SkillType::Skill => skill = *lvl,
                        SkillType::Burst => burst = *lvl,
                    }
                }

                if level < settings.min_character_level
                    || ascension < settings.min_character_ascension
                    || constellation < settings.min_character_constellation
                {
                    return None;
                }

                Some(GoodCharacter {
                    key: to_good_key(name),
                    level: level as i32,
                    constellation: constellation as i32,
                    ascension: ascension as i32,
                    talent: GoodTalent {
                        auto: auto as i32,
                        skill: skill as i32,
                        burst: burst as i32,
                    },
                    element: None,
                })
            })
            .collect()
    }

    fn export_artifacts(&self, settings: &CaptureExportSettings) -> Vec<GoodArtifact> {
        self.items
            .iter()
            .filter_map(|item| {
                if !item.has_equip() {
                    return None;
                }
                let equip = item.equip();
                let location = self.resolve_location(item.guid);

                if !equip.has_reliquary() {
                    return None;
                }
                let artifact_data = self.data_cache.get_artifact(item.item_id)?;
                let artifact = equip.reliquary();

                // Accumulate substats: group by property, sum values, track initial + per-roll
                let mut substats: IndexMap<Property, (f64, f64, Vec<f64>)> = IndexMap::new();
                for substat_id in artifact.append_prop_id_list.iter() {
                    let substat = self.data_cache.get_affix(*substat_id)?;
                    let entry = substats
                        .entry(substat.property)
                        .or_insert((0.0, substat.value, Vec::new()));
                    entry.0 += substat.value;
                    entry.2.push(substat.value);
                }
                let substats: Vec<GoodSubStat> = substats
                    .into_iter()
                    .map(|(property, (value, initial_value, rolls))| GoodSubStat {
                        key: property.good_name().to_string(),
                        value: round_stat(property, value),
                        initial_value: Some(round_stat(property, initial_value)),
                        rolls,
                    })
                    .collect();

                let unactivated_substats: Vec<GoodSubStat> = artifact
                    .unactivated_prop_id_list
                    .iter()
                    .filter_map(|substat_id| {
                        let substat = self.data_cache.get_affix(*substat_id)?;
                        let rounded = round_stat(substat.property, substat.value);
                        Some(GoodSubStat {
                            key: substat.property.good_name().to_string(),
                            value: rounded,
                            initial_value: Some(rounded),
                            rolls: vec![substat.value],
                        })
                    })
                    .collect();

                let total_rolls = artifact.append_prop_id_list.len() as i32;
                let level = (artifact.level - 1) as i32;
                let rarity = artifact_data.rarity as i32;
                let astral_mark = artifact.starred;
                let elixir_crafted = !artifact.elixer_choices.is_empty();
                let main_stat_key = self
                    .data_cache
                    .get_property(artifact.main_prop_id)?
                    .good_name()
                    .to_string();

                if (level as u32) < settings.min_artifact_level
                    || (rarity as u32) < MIN_ARTIFACT_RARITY
                {
                    return None;
                }

                // Skip artifacts below their set's max rarity (e.g. 4★ of a 5★ set).
                // Use the hardcoded MAX_4STAR_ARTIFACT_SETS list as ground truth for
                // 4★-max sets, since data_cache incorrectly reports some as rarity 5.
                let good_key = to_good_key(&artifact_data.set);
                let true_max_rarity = if MAX_4STAR_ARTIFACT_SETS.contains(&good_key.as_str()) {
                    4u32
                } else {
                    self.data_cache
                        .artifact_set_max_rarity(&artifact_data.set)
                        .unwrap_or(5)
                };
                if (rarity as u32) < true_max_rarity {
                    return None;
                }

                Some(GoodArtifact {
                    set_key: to_good_key(&artifact_data.set),
                    slot_key: artifact_data.slot.good_name().to_string(),
                    level,
                    rarity,
                    main_stat_key,
                    substats,
                    location,
                    lock: equip.is_locked,
                    astral_mark,
                    elixir_crafted,
                    unactivated_substats,
                    total_rolls: Some(total_rolls),
                })
            })
            .collect()
    }

    fn export_weapons(&self, settings: &CaptureExportSettings) -> Vec<GoodWeapon> {
        self.items
            .iter()
            .filter_map(|item| {
                if !item.has_equip() {
                    return None;
                }
                let equip = item.equip();
                let location = self.resolve_location(item.guid);

                if !equip.has_weapon() {
                    return None;
                }
                let weapon_data = self.data_cache.get_weapon(item.item_id)?;
                let weapon = equip.weapon();
                let refinement =
                    weapon.affix_map.values().cloned().next().unwrap_or_default() + 1;

                let level = weapon.level;
                let ascension = weapon.promote_level;

                if level < settings.min_weapon_level
                    || refinement < settings.min_weapon_refinement
                    || ascension < settings.min_weapon_ascension
                    || weapon_data.rarity < MIN_WEAPON_RARITY
                {
                    return None;
                }

                Some(GoodWeapon {
                    key: to_good_key(&weapon_data.name),
                    level: level as i32,
                    ascension: ascension as i32,
                    refinement: refinement as i32,
                    rarity: weapon_data.rarity as i32,
                    location,
                    lock: equip.is_locked,
                })
            })
            .collect()
    }
}

/// Round a stat value the same way the game does (f64 half-up rounding).
///
/// Note: Rust's `f64::round()` is actually round-half-away-from-zero (correct
/// for positive values), but we use the explicit floor(x+0.5) form to make the
/// intent clear. Verified against 274 rounding boundary cases from packet
/// capture vs OCR — the game uses half-up, not banker's rounding.
///
/// Percentages round to 1 decimal; flat stats round to integers.
fn round_stat(property: Property, value: f64) -> f64 {
    if property.is_percentage() {
        (value * 10.0 + 0.5 + 1e-9).floor() / 10.0
    } else {
        (value + 0.5 + 1e-9).floor()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_stat_percentage() {
        assert_eq!(round_stat(Property::CritRate, 3.89), 3.9);
        assert_eq!(round_stat(Property::CritDamage, 7.75), 7.8);
        assert_eq!(round_stat(Property::HpPercent, 5.25), 5.3);
        assert_eq!(round_stat(Property::AttackPercent, 5.24), 5.2);
    }

    #[test]
    fn round_stat_flat() {
        assert_eq!(round_stat(Property::Hp, 298.7), 299.0);
        assert_eq!(round_stat(Property::Attack, 19.4), 19.0);
        assert_eq!(round_stat(Property::Defense, 23.5), 24.0);
        assert_eq!(round_stat(Property::ElementalMastery, 16.3), 16.0);
    }

    #[test]
    fn round_stat_half_up_not_bankers() {
        // 62.5 must round to 63 (half-up), not 62 (banker's round-to-even)
        // Real case: DEF rolls 16.20 + 23.15 + 23.15 = 62.50
        assert_eq!(round_stat(Property::Defense, 62.5), 63.0);
        // Percentage: 5.25 must round to 5.3 (half-up), not 5.2 (banker's)
        assert_eq!(round_stat(Property::HpPercent, 5.25), 5.3);
    }

    #[test]
    fn export_default_settings() {
        let data_cache = DataCache {
            version: 1,
            git_hash: String::new(),
            affix_map: HashMap::new(),
            artifact_map: HashMap::new(),
            character_map: HashMap::new(),
            material_map: HashMap::new(),
            property_map: HashMap::new(),
            set_map: HashMap::new(),
            skill_type_map: HashMap::new(),
            weapon_map: HashMap::new(),
        };
        let player = PlayerData::new(data_cache);
        let export = player.export(&CaptureExportSettings::default()).unwrap();
        assert_eq!(export.format, "GOOD");
        assert_eq!(export.version, 3);
        // Empty data → empty lists
        assert_eq!(export.characters.unwrap().len(), 0);
        assert_eq!(export.weapons.unwrap().len(), 0);
        assert_eq!(export.artifacts.unwrap().len(), 0);
    }

    #[test]
    fn export_respects_include_flags() {
        let data_cache = DataCache {
            version: 1,
            git_hash: String::new(),
            affix_map: HashMap::new(),
            artifact_map: HashMap::new(),
            character_map: HashMap::new(),
            material_map: HashMap::new(),
            property_map: HashMap::new(),
            set_map: HashMap::new(),
            skill_type_map: HashMap::new(),
            weapon_map: HashMap::new(),
        };
        let player = PlayerData::new(data_cache);
        let settings = CaptureExportSettings {
            include_characters: false,
            include_weapons: false,
            include_artifacts: true,
            ..Default::default()
        };
        let export = player.export(&settings).unwrap();
        assert!(export.characters.is_none());
        assert!(export.weapons.is_none());
        assert!(export.artifacts.is_some());
    }
}
