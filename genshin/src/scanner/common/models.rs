use serde::{Deserialize, Serialize};

/// GOOD v3 character export
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoodCharacter {
    pub key: String,
    pub level: i32,
    pub constellation: i32,
    pub ascension: i32,
    pub talent: GoodTalent,
    /// Element for multi-element characters (Traveler, Manekin, Manekina).
    /// Only serialized when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoodTalent {
    pub auto: i32,
    pub skill: i32,
    pub burst: i32,
}

/// GOOD v3 weapon export
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoodWeapon {
    pub key: String,
    pub level: i32,
    pub ascension: i32,
    pub refinement: i32,
    #[serde(default, skip_serializing)]
    pub rarity: i32,
    pub location: String,
    pub lock: bool,
}

/// GOOD v3 artifact export
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoodArtifact {
    #[serde(rename = "setKey")]
    pub set_key: String,
    #[serde(rename = "slotKey")]
    pub slot_key: String,
    pub level: i32,
    pub rarity: i32,
    #[serde(rename = "mainStatKey")]
    pub main_stat_key: String,
    pub substats: Vec<GoodSubStat>,
    pub location: String,
    pub lock: bool,
    #[serde(default, rename = "astralMark")]
    pub astral_mark: bool,
    #[serde(default, alias = "elixerCrafted", rename = "elixirCrafted")]
    pub elixir_crafted: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty", rename = "unactivatedSubstats")]
    pub unactivated_substats: Vec<GoodSubStat>,
    /// Total number of substat rolls (initial + upgrades). Set by the roll solver.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "totalRolls")]
    pub total_rolls: Option<i32>,
}

/// Substat in GOOD v3 format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoodSubStat {
    pub key: String,
    pub value: f64,
    /// Value of the initial roll (before upgrades). Set by the roll solver
    /// when the substat has exactly 1 roll (i.e., the value IS the initial roll).
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "initialValue")]
    pub initial_value: Option<f64>,
    /// Per-roll raw values from game packet data (capture only, not set by OCR scanner).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rolls: Vec<f64>,
}

/// GOOD v3 full export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoodExport {
    pub format: String,
    pub version: u32,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub characters: Option<Vec<GoodCharacter>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weapons: Option<Vec<GoodWeapon>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<GoodArtifact>>,
}

impl GoodExport {
    pub fn new(
        characters: Option<Vec<GoodCharacter>>,
        weapons: Option<Vec<GoodWeapon>>,
        artifacts: Option<Vec<GoodArtifact>>,
    ) -> Self {
        Self {
            format: "GOOD".to_string(),
            version: 3,
            source: "yas-GOODScanner".to_string(),
            characters,
            weapons,
            artifacts,
        }
    }
}

/// Debug info for a single OCR field.
#[derive(Debug)]
pub struct DebugOcrField {
    /// Field name (e.g., "weapon_name", "level")
    pub field_name: String,
    /// Raw OCR output text
    pub raw_text: String,
    /// Parsed/matched value
    pub parsed_value: String,
    /// OCR region used (x, y, w, h) at 1920x1080 base
    pub region: (f64, f64, f64, f64),
    /// Time taken for this OCR call in milliseconds
    pub duration_ms: u64,
}

/// Debug result of scanning a single item.
#[derive(Debug)]
pub struct DebugScanResult {
    /// Per-field OCR results
    pub fields: Vec<DebugOcrField>,
    /// Total scan time in milliseconds
    pub total_duration_ms: u64,
    /// The parsed item data as a serializable string
    pub parsed_json: String,
}
