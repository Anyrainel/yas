use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{bail, Result};
use image::{GenericImageView, RgbImage};
use indicatif::{ProgressBar, ProgressStyle};
use yas::{log_debug, log_error, log_info, log_warn};
use regex::Regex;

use yas::ocr::ImageToText;
use yas::utils;

use super::GoodCharacterScannerConfig;
use crate::scanner::common::constants::*;
use crate::scanner::common::coord_scaler::CoordScaler;
use crate::scanner::common::annotator;
use crate::scanner::common::debug_dump::level_cap_display;
use crate::scanner::common::fuzzy_match::fuzzy_match_map_pair;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::MappingManager;
use crate::scanner::common::models::{DebugOcrField, DebugScanResult, GoodCharacter, GoodTalent};
use crate::scanner::common::ocr_factory;
use crate::scanner::common::ocr_pool::{OcrPool, SharedOcrPools};
use crate::scanner::common::stat_parser::level_to_ascension;

// ── Data Structures ─────────────────────────────────────────────────────────

/// Extra metadata from scanning a character, used for suspicious-result detection.
/// Kept separate from `GoodCharacter` (which is the serialized output).
struct ScanMeta {
    /// True if `adjust_talents()` hit a raw value < 4 during constellation subtraction.
    talent_suspicious: bool,
    /// Raw OCR'd skill level BEFORE constellation/Tartaglia adjustment.
    raw_skill: i32,
    /// Raw OCR'd burst level BEFORE constellation/Tartaglia adjustment.
    raw_burst: i32,
}

/// Phase 1 captures: images from deterministic tab sequence (no fallbacks).
struct ScanCaptures {
    /// Position in the character roster (0-based).
    viewed_index: usize,
    name: String,
    element: Option<String>,
    raw_name_text: String,
    attrs_image: RgbImage,
    /// None for no-constellation characters (Aloy, Manekin, Manekina).
    constellation_image: Option<RgbImage>,
    talents_image: RgbImage,
}

/// Phase 2 captures: full rescan with constellation node + talent detail images.
struct RescanCaptures {
    /// Index in the `characters` vec.
    char_index: usize,
    name: String,
    old: GoodCharacter,
    attrs_image: RgbImage,
    constellation_tab_image: RgbImage,
    /// One image per constellation node (C1–C6), each showing activation status popup.
    constellation_node_images: Vec<RgbImage>,
    talent_overview_image: RgbImage,
    /// One image per talent detail (auto, skill, burst).
    talent_detail_images: Vec<RgbImage>,
}

/// Worker input.
enum CharacterWork {
    Scan(ScanCaptures),
    Rescan(RescanCaptures),
    /// Signals end of a phase; worker sends PhaseDone after processing remaining items.
    Done,
}

/// Worker output.
enum CharacterResult {
    Scanned {
        viewed_index: usize,
        character: Option<GoodCharacter>,
        meta: ScanMeta,
    },
    Rescanned {
        char_index: usize,
        character: GoodCharacter,
    },
    /// Sent after all items in the current phase have been processed.
    PhaseDone,
}

// ── Constants ────────────────────────────────────────────────────────────────

/// Tip logged after OCR failures that are likely caused by slow UI transitions.
const DELAY_TIP: &str = "[tip] 如果扫描器切换过快，请在设置中增大「面板切换」或「切换角色」延迟 / \
    [tip] If the scanner moves too fast, try increasing the \"Panel switch\" or \"Next character\" delay in settings";

// ── Scanner ──────────────────────────────────────────────────────────────────

pub struct GoodCharacterScanner {
    config: GoodCharacterScannerConfig,
    mappings: Arc<MappingManager>,
}

impl GoodCharacterScanner {
    pub fn new(
        config: GoodCharacterScannerConfig,
        mappings: Arc<MappingManager>,
    ) -> Result<Self> {
        Ok(Self { config, mappings })
    }
}

// ── Static helpers ───────────────────────────────────────────────────────────

impl GoodCharacterScanner {
    /// OCR a region from an already-captured image (no new capture).
    fn ocr_image_region(
        ocr: &dyn ImageToText<RgbImage>,
        image: &RgbImage,
        rect: (f64, f64, f64, f64),
        scaler: &CoordScaler,
    ) -> Result<String> {
        let (bx, by, bw, bh) = rect;
        let x = scaler.x(bx) as u32;
        let y = scaler.y(by) as u32;
        let w = scaler.x(bw) as u32;
        let h = scaler.y(bh) as u32;

        let x = x.min(image.width().saturating_sub(1));
        let y = y.min(image.height().saturating_sub(1));
        let w = w.min(image.width().saturating_sub(x));
        let h = h.min(image.height().saturating_sub(y));

        if w == 0 || h == 0 {
            return Ok(String::new());
        }

        let sub = image.view(x, y, w, h).to_image();
        let text = ocr.image_to_text(&sub, false)?;
        Ok(text.trim().to_string())
    }

    /// Characters that use the element field (multi-element or renameable).
    const ELEMENT_CHARACTERS: &'static [&'static str] = &["Traveler", "Manekin", "Manekina"];

    /// Map Chinese element name to English GOOD element key.
    fn zh_element_to_good(zh: &str) -> Option<String> {
        match zh.trim() {
            "\u{706B}" => Some("Pyro".into()),       // 火
            "\u{6C34}" => Some("Hydro".into()),      // 水
            "\u{96F7}" => Some("Electro".into()),    // 雷
            "\u{51B0}" => Some("Cryo".into()),       // 冰
            "\u{98CE}" => Some("Anemo".into()),      // 风
            "\u{5CA9}" => Some("Geo".into()),        // 岩
            "\u{8349}" => Some("Dendro".into()),     // 草
            _ => None,
        }
    }

    /// Parse character name and element from OCR text.
    /// Text format: "Element/CharacterName" (e.g., "冰/神里绫华")
    /// Returns (good_key, element, entity_name).
    fn parse_name_and_element(&self, text: &str) -> (Option<String>, Option<String>, Option<String>) {
        if text.is_empty() {
            return (None, None, None);
        }

        let slash_char = if text.contains('/') { Some('/') } else if text.contains('\u{FF0F}') { Some('\u{FF0F}') } else { None };
        if let Some(slash) = slash_char {
            let idx = text.find(slash).unwrap();
            let element = text[..idx].trim().to_string();
            let raw_name: String = text[idx + slash.len_utf8()..]
                .chars()
                .filter(|c| {
                    matches!(*c, '\u{4E00}'..='\u{9FFF}' | '\u{300C}' | '\u{300D}' | 'a'..='z' | 'A'..='Z' | '0'..='9')
                })
                .collect();
            let pair = fuzzy_match_map_pair(&raw_name, &self.mappings.character_name_map);
            let (entity_name, good_key) = match pair {
                Some((n, k)) => (Some(n), Some(k)),
                None => (None, None),
            };
            (good_key, Some(element), entity_name)
        } else {
            let pair = fuzzy_match_map_pair(text, &self.mappings.character_name_map);
            let (entity_name, good_key) = match pair {
                Some((n, k)) => (Some(n), Some(k)),
                None => (None, None),
            };
            (good_key, None, entity_name)
        }
    }

    /// Valid level caps in Genshin Impact.
    const VALID_MAX_LEVELS: &'static [i32] = &[20, 40, 50, 60, 70, 80, 90, 95, 100];

    /// Minimum level for each cap (the previous cap, i.e. you must reach it to ascend).
    /// Index corresponds to VALID_MAX_LEVELS.
    const MIN_LEVEL_FOR_CAP: &'static [i32] = &[1, 20, 40, 50, 60, 70, 80, 90, 95];

    /// Finalize a (level, max) pair: snap max to nearest valid cap, compute ascended flag.
    /// Does NOT snap level — invalid levels (91-94, 96-99) are preserved so they
    /// can be detected as OCR errors and trigger a rescan.
    fn finalize_level(level: i32, max_level: i32) -> (i32, bool) {
        let max_level = Self::VALID_MAX_LEVELS
            .iter()
            .copied()
            .min_by_key(|&v| (v - max_level).unsigned_abs())
            .unwrap_or(max_level);
        let level = if max_level >= 95 { max_level } else { level.min(max_level) };
        let ascended = level >= 20 && level < max_level;
        (level, ascended)
    }

    /// Check if a (level, max) pair is plausible.
    fn is_level_plausible(level: i32, max_level: i32) -> bool {
        if level > max_level || level < 1 {
            return false;
        }
        if let Some(idx) = Self::VALID_MAX_LEVELS.iter().position(|&v| v == max_level) {
            let min_lv = Self::MIN_LEVEL_FOR_CAP[idx];
            level >= min_lv
        } else {
            true
        }
    }

    /// Try to split a digit string into (level, max) pair.
    fn try_split_digits(digits: &str) -> Option<(i32, i32)> {
        for i in (1..digits.len()).rev() {
            if let (Ok(lv), Ok(mx)) = (digits[..i].parse::<i32>(), digits[i..].parse::<i32>()) {
                if (1..=100).contains(&lv) && (10..=100).contains(&mx) && mx >= lv {
                    return Some((lv, mx));
                }
            }
        }
        None
    }

    /// Derive the effective max level (cap) from a level reading.
    fn derive_max_level(level: i32, ascended: bool) -> i32 {
        if ascended {
            Self::VALID_MAX_LEVELS.iter().copied().find(|&v| v > level).unwrap_or(100)
        } else if Self::VALID_MAX_LEVELS.contains(&level) {
            level
        } else {
            Self::VALID_MAX_LEVELS.iter().copied().find(|&v| v > level).unwrap_or(100)
        }
    }

    /// Check if a level reading looks suspicious and warrants a retry.
    fn is_level_suspicious(level: i32, ascended: bool) -> bool {
        if (91..=94).contains(&level) || (96..=99).contains(&level) {
            return true;
        }
        if level >= 2 && level < 10 {
            return true;
        }
        let max_level = Self::derive_max_level(level, ascended);
        if !Self::is_level_plausible(level, max_level) {
            return true;
        }
        false
    }

    /// Parse "Lv.X" format from talent overview text.
    ///
    /// Tolerant of OCR errors: accepts Lv, LV, Ly, lv with ./:/ /no separator
    fn parse_lv_text(text: &str) -> i32 {
        if text.is_empty() {
            return 0;
        }
        let clean: String = {
            let chars: Vec<char> = text.chars().collect();
            let mut result = String::with_capacity(text.len());
            for (i, &c) in chars.iter().enumerate() {
                if c == ' ' && i > 0 && i + 1 < chars.len()
                    && chars[i - 1].is_ascii_digit() && chars[i + 1].is_ascii_digit()
                {
                    continue;
                }
                result.push(c);
            }
            result
        };
        let re = Regex::new(r"(?i)[Ll][VvYy][.:．]?\s*(\d{1,2})").unwrap();
        if let Some(caps) = re.captures(&clean) {
            let lv: i32 = caps[1].parse().unwrap_or(0);
            if (1..=15).contains(&lv) {
                return lv;
            }
        }
        let re2 = Regex::new(r"(\d{1,2})").unwrap();
        if let Some(caps) = re2.captures(&clean) {
            let lv: i32 = caps[1].parse().unwrap_or(0);
            if (1..=15).contains(&lv) {
                return lv;
            }
        }
        0
    }

    /// Apply Tartaglia, constellation, and Traveler talent adjustments.
    ///
    /// Returns (auto, skill, burst, suspicious):
    /// - `suspicious` is true if any talent that should have a +3 bonus
    ///   reads below 4 (meaning the OCR value is too low to subtract from).
    fn adjust_talents(
        &self,
        raw_auto: i32,
        raw_skill: i32,
        raw_burst: i32,
        name: &str,
        constellation: i32,
    ) -> (i32, i32, i32, bool) {
        let mut auto = raw_auto;
        let mut skill = raw_skill;
        let mut burst = raw_burst;
        let mut suspicious = false;

        if name == TARTAGLIA_KEY {
            auto = (auto - 1).max(1);
        }

        let sub3 = |val: &mut i32, sus: &mut bool| {
            if *val < 4 {
                *sus = true;
            }
            *val = (*val - 3).max(1);
        };

        if let Some(bonus) = self.mappings.character_const_bonus.get(name) {
            if constellation >= 3 {
                if let Some(ref c3_type) = bonus.c3 {
                    match c3_type.as_str() {
                        "A" => sub3(&mut auto, &mut suspicious),
                        "E" => sub3(&mut skill, &mut suspicious),
                        "Q" => sub3(&mut burst, &mut suspicious),
                        _ => {}
                    }
                }
            }
            if constellation >= 5 {
                if let Some(ref c5_type) = bonus.c5 {
                    match c5_type.as_str() {
                        "A" => sub3(&mut auto, &mut suspicious),
                        "E" => sub3(&mut skill, &mut suspicious),
                        "Q" => sub3(&mut burst, &mut suspicious),
                        _ => {}
                    }
                }
            }
        } else if name == "Traveler" {
            if constellation >= 5 {
                sub3(&mut skill, &mut suspicious);
                sub3(&mut burst, &mut suspicious);
            } else {
                if skill > 10 { sub3(&mut skill, &mut suspicious); }
                if burst > 10 { sub3(&mut burst, &mut suspicious); }
            }
        }

        (auto, skill, burst, suspicious)
    }

    /// Get the maximum allowed base talent level for an ascension phase (0–6).
    fn max_talent_for_ascension(ascension: i32) -> i32 {
        match ascension {
            0 => 1,
            1 => 1,
            2 => 2,
            3 => 4,
            4 => 6,
            5 => 8,
            _ => 10,
        }
    }

    /// Check if a scanned character has suspicious results that warrant a rescan.
    fn is_character_suspicious(c: &GoodCharacter, meta: Option<&ScanMeta>) -> bool {
        let ascended = false;
        if Self::is_level_suspicious(c.level, ascended) {
            return true;
        }
        if c.level >= 40 {
            if let Some(m) = meta {
                if m.raw_skill == 1 || m.raw_burst == 1 {
                    return true;
                }
            }
        }
        let max_talent = Self::max_talent_for_ascension(c.ascension);
        if c.talent.auto > max_talent || c.talent.skill > max_talent || c.talent.burst > max_talent {
            return true;
        }
        if let Some(m) = meta {
            if m.talent_suspicious {
                return true;
            }
        }
        false
    }
}

// ── Image-based OCR methods (no controller needed) ───────────────────────────

impl GoodCharacterScanner {
    /// OCR name and element from a captured attributes image. Returns (key, element, raw_text).
    /// Tries v5 first (better on names), then v4. Does NOT retry.
    fn read_name_from_image(
        &self,
        v5_ocr: Option<&dyn ImageToText<RgbImage>>,
        v4_ocr: &dyn ImageToText<RgbImage>,
        image: &RgbImage,
        scaler: &CoordScaler,
    ) -> (Option<String>, Option<String>, String) {
        if let Some(v5) = v5_ocr {
            if let Ok(text) = Self::ocr_image_region(v5, image, CHAR_NAME_RECT, scaler) {
                let (name, element, entity) = self.parse_name_and_element(&text);
                if name.is_some() {
                    log_debug!("[character] 名字OCR: {:?} -> {} ({})", "[character] name OCR: {:?} -> {} ({})",
                        text, entity.as_deref().unwrap_or("?"), name.as_deref().unwrap_or("?"));
                    return (name, element, text);
                }
            }
        }
        if let Ok(text) = Self::ocr_image_region(v4_ocr, image, CHAR_NAME_RECT, scaler) {
            let (name, element, entity) = self.parse_name_and_element(&text);
            if name.is_some() {
                log_debug!("[character] 名字OCR: {:?} -> {} ({})", "[character] name OCR: {:?} -> {} ({})",
                    text, entity.as_deref().unwrap_or("?"), name.as_deref().unwrap_or("?"));
                return (name, element, text);
            }
            // Return raw text even on match failure for error reporting
            return (None, None, text);
        }
        (None, None, String::new())
    }

    /// Returns (level, ascended, raw_ocr_text).
    fn read_level_from_image(
        ocr: &dyn ImageToText<RgbImage>,
        image: &RgbImage,
        scaler: &CoordScaler,
    ) -> (i32, bool, String) {
        let text = match Self::ocr_image_region(ocr, image, CHAR_LEVEL_RECT, scaler) {
            Ok(t) => t,
            Err(_) => return (1, false, String::new()),
        };

        let (level, ascended) = Self::parse_level_text(&text);
        (level, ascended, text)
    }

    /// Parse level from already-OCR'd text. Returns (level, ascended).
    fn parse_level_text(text: &str) -> (i32, bool) {
        // Try standard "XX/YY" format
        let re = Regex::new(r"(\d+)\s*[./·:]*\s*/\s*[./·:]*\s*(\d+)").unwrap();
        if let Some(caps) = re.captures(text) {
            let level: i32 = caps[1].parse().unwrap_or(1);
            let raw_max: i32 = caps[2].parse().unwrap_or(20);
            return Self::finalize_level(level, raw_max);
        }

        // Fallback: extract all digit characters and try to split
        let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let raw: i64 = digits.parse().unwrap_or(0);
            if raw > 0 && raw <= 100 {
                return (raw as i32, false);
            }

            // Phase 1: clean split
            if let Some((lv, mx)) = Self::try_split_digits(&digits) {
                log_debug!("[character] 等级OCR回退拆分: {:?} -> {}/{}", "[character] level OCR fallback split: {:?} -> {}/{}", digits, lv, mx);
                return Self::finalize_level(lv, mx);
            }

            // Phase 2: remove one noise char at each position
            {
                let mid = digits.len() as f64 / 2.0;
                let mut best_noise: Option<(i32, i32, usize, f64)> = None;
                for remove_idx in 0..digits.len() {
                    let reduced: String = digits
                        .char_indices()
                        .filter(|&(i, _)| i != remove_idx)
                        .map(|(_, c)| c)
                        .collect();
                    if let Some((lv, mx)) = Self::try_split_digits(&reduced) {
                        let dist = (remove_idx as f64 - mid).abs();
                        if best_noise.is_none() || dist < best_noise.unwrap().3 {
                            best_noise = Some((lv, mx, remove_idx, dist));
                        }
                    }
                }
                if let Some((lv, mx, idx, _)) = best_noise {
                    log_debug!(
                        "[character] 等级OCR去噪拆分: {:?} (移除索引 {}) -> {}/{}",
                        "[character] level OCR noise-remove split: {:?} (remove idx {}) -> {}/{}",
                        digits, idx, lv, mx
                    );
                    return Self::finalize_level(lv, mx);
                }
            }

            // Phase 3: take first 2-3 digits as level
            for len in [3, 2] {
                if digits.len() >= len {
                    if let Ok(lv) = digits[..len].parse::<i32>() {
                        if (1..=100).contains(&lv) {
                            log_debug!("[character] 等级OCR部分提取: {:?} -> {}", "[character] level OCR partial extract: {:?} -> {}", digits, lv);
                            return (lv, false);
                        }
                    }
                }
            }
        }

        log_warn!("[character] 等级OCR完全失败: {:?}", "[character] level OCR completely failed: {:?}", text);
        (1, false)
    }

    /// OCR talent overview levels from a captured talents image (parallel via rayon).
    /// Returns ((auto, skill, burst), (raw_auto, raw_skill, raw_burst)).
    /// Level 0 means OCR failed for that talent; raw text is empty on failure.
    fn read_talents_from_image(
        ocr_pool: &OcrPool,
        image: &RgbImage,
        character_name: &str,
        scaler: &CoordScaler,
    ) -> ((i32, i32, i32), (String, String, String)) {
        let has_special = SPECIAL_BURST_CHARACTERS.contains(&character_name);
        let burst_rect = if has_special {
            CHAR_TALENT_OVERVIEW_BURST_SPECIAL
        } else {
            CHAR_TALENT_OVERVIEW_BURST
        };

        let (auto_res, (skill_res, burst_res)) = rayon::join(
            || {
                let ocr = ocr_pool.get();
                Self::ocr_image_region(&ocr, image, CHAR_TALENT_OVERVIEW_AUTO, scaler)
                    .map(|t| {
                        let lv = Self::parse_lv_text(&t);
                        (lv, t)
                    })
                    .unwrap_or((0, String::new()))
            },
            || {
                rayon::join(
                    || {
                        let ocr = ocr_pool.get();
                        Self::ocr_image_region(&ocr, image, CHAR_TALENT_OVERVIEW_SKILL, scaler)
                            .map(|t| {
                                let lv = Self::parse_lv_text(&t);
                                (lv, t)
                            })
                            .unwrap_or((0, String::new()))
                    },
                    || {
                        let ocr = ocr_pool.get();
                        Self::ocr_image_region(&ocr, image, burst_rect, scaler)
                            .map(|t| {
                                let lv = Self::parse_lv_text(&t);
                                (lv, t)
                            })
                            .unwrap_or((0, String::new()))
                    },
                )
            },
        );

        log_debug!("[talent] 概览: 普攻={} 战技={} 爆发={}", "[talent] overview: auto={} skill={} burst={}",
            auto_res.0, skill_res.0, burst_res.0);

        (
            (auto_res.0, skill_res.0, burst_res.0),
            (auto_res.1, skill_res.1, burst_res.1),
        )
    }

    /// Check if a constellation node is activated from its detail-view capture.
    /// OCRs the activation status region and looks for "已激活".
    fn check_activation_from_image(
        ocr: &dyn ImageToText<RgbImage>,
        image: &RgbImage,
        scaler: &CoordScaler,
    ) -> bool {
        let text = Self::ocr_image_region(ocr, image, CHAR_CONSTELLATION_ACTIVATE_RECT, scaler)
            .unwrap_or_default();
        text.contains("\u{5DF2}\u{6FC0}\u{6D3B}") // 已激活
    }

    /// OCR talent level from a talent detail-view capture.
    /// Looks for "Lv.X" pattern in the detail header region.
    fn read_talent_from_detail_image(
        ocr: &dyn ImageToText<RgbImage>,
        image: &RgbImage,
        scaler: &CoordScaler,
    ) -> i32 {
        let text = match Self::ocr_image_region(ocr, image, CHAR_TALENT_LEVEL_RECT, scaler) {
            Ok(t) => t,
            Err(_) => return 0,
        };
        let re = Regex::new(r"[Ll][Vv]\.?\s*(\d{1,2})").unwrap();
        if let Some(caps) = re.captures(&text) {
            let v: i32 = caps[1].parse().unwrap_or(0);
            if (1..=15).contains(&v) {
                return v;
            }
        }
        let re2 = Regex::new(r"(\d{1,2})").unwrap();
        if let Some(caps) = re2.captures(&text) {
            let v: i32 = caps[1].parse().unwrap_or(0);
            if (1..=15).contains(&v) {
                return v;
            }
        }
        0
    }
}

// ── Worker processing functions ──────────────────────────────────────────────

impl GoodCharacterScanner {
    /// Process Phase 1 captures on the worker thread.
    ///
    /// Pure image processing: OCR level, pixel constellation, OCR talents.
    /// No game controller access.
    fn process_scan_captures(
        &self,
        captures: ScanCaptures,
        ocr_pool: &OcrPool,
        scaler: &CoordScaler,
    ) -> CharacterResult {
        let ScanCaptures {
            viewed_index,
            name,
            element,
            raw_name_text,
            attrs_image,
            constellation_image,
            talents_image,
        } = captures;

        // -- Annotator: begin item --
        annotator::begin_item("characters", viewed_index, scaler);

        // -- Attributes image: name + level --
        annotator::add_image("attributes", &attrs_image);
        annotator::record_ocr("name", CHAR_NAME_RECT, &raw_name_text);
        annotator::set_final("name", &name);
        // Only show resolved name if it differs from what's already in the raw text.
        // Raw format is "Element/CharName" — if CharName matches, raw is clear enough.
        let cn_name = self.mappings.character_name_map.iter()
            .find(|(_, v)| v.as_str() == name)
            .map(|(cn, _)| cn.as_str())
            .unwrap_or(&name);
        let raw_after_slash = raw_name_text.split(['/', '／']).last().unwrap_or("").trim();
        if raw_after_slash != cn_name {
            annotator::set_display("name", cn_name);
        }

        let ocr = ocr_pool.get();
        let (level, ascended, raw_level_text) = Self::read_level_from_image(&ocr, &attrs_image, scaler);
        let lvl_display = level_cap_display(level, ascended);
        annotator::record_ocr("level", CHAR_LEVEL_RECT, &raw_level_text);
        annotator::set_final("level", &lvl_display);

        if Self::is_level_suspicious(level, ascended) {
            log_debug!(
                "[character] 等级 {} (突破={}) 可能需要重新读取",
                "[character] level {} (ascended={}) may need re-reading",
                level, ascended
            );
        }

        // -- Constellation image: pixel detection (no fallback) --
        let constellation = if let Some(ref const_image) = constellation_image {
            annotator::add_image("constellation", const_image);
            let result = crate::scanner::common::pixel_utils::detect_constellation_pixel(
                const_image, scaler,
            );
            if !result.monotonic {
                log_debug!(
                    "[constellation] 像素非单调 {}，将在第二轮重新扫描",
                    "[constellation] pixel non-monotonic for {}, will rescan in phase 2",
                    name
                );
            }
            result.level
        } else {
            0
        };

        // -- Talents image: overview OCR (no click fallback) --
        drop(ocr);
        annotator::add_image("talents", &talents_image);
        let ((auto_lv, skill_lv, burst_lv), (raw_auto, raw_skill, raw_burst)) =
            Self::read_talents_from_image(ocr_pool, &talents_image, &name, scaler);

        let auto = if auto_lv > 0 { auto_lv } else { 1 };
        let skill = if skill_lv > 0 { skill_lv } else { 1 };
        let burst = if burst_lv > 0 { burst_lv } else { 1 };

        if auto_lv == 0 || skill_lv == 0 || burst_lv == 0 {
            let mut missing = Vec::new();
            if auto_lv == 0 { missing.push("auto"); }
            if skill_lv == 0 { missing.push("skill"); }
            if burst_lv == 0 { missing.push("burst"); }
            log_debug!(
                "[character] 天赋概览失败: {}，将在第二轮使用点击回退",
                "[character] talent overview failed for: {}, will use click fallback in phase 2",
                missing.join("/")
            );
        }

        // Record talent annotations with raw OCR text
        let has_special = SPECIAL_BURST_CHARACTERS.contains(&name.as_str());
        let burst_rect = if has_special { CHAR_TALENT_OVERVIEW_BURST_SPECIAL } else { CHAR_TALENT_OVERVIEW_BURST };
        annotator::record_ocr("talent_auto", CHAR_TALENT_OVERVIEW_AUTO, &raw_auto);
        annotator::set_final("talent_auto", &format!("{}", auto));
        annotator::record_ocr("talent_skill", CHAR_TALENT_OVERVIEW_SKILL, &raw_skill);
        annotator::set_final("talent_skill", &format!("{}", skill));
        annotator::record_ocr("talent_burst", burst_rect, &raw_burst);
        annotator::set_final("talent_burst", &format!("{}", burst));

        // -- Build character --
        let ascension = level_to_ascension(level, ascended);
        let (adj_auto, adj_skill, adj_burst, talent_suspicious) =
            self.adjust_talents(auto, skill, burst, &name, constellation);

        let good_element = if Self::ELEMENT_CHARACTERS.contains(&name.as_str()) {
            element.as_deref().and_then(Self::zh_element_to_good)
        } else {
            None
        };

        let character = GoodCharacter {
            key: name,
            level,
            constellation,
            ascension,
            talent: GoodTalent {
                auto: adj_auto,
                skill: adj_skill,
                burst: adj_burst,
            },
            element: good_element,
        };

        annotator::record_ocr("constellation_final", (0.0, 0.0, 0.0, 0.0), "");
        annotator::set_final("constellation_final", &format!("C{}", constellation));
        annotator::record_ocr("talents_adjusted", (0.0, 0.0, 0.0, 0.0), "");
        annotator::set_final("talents_adjusted",
            &format!("auto={} skill={} burst={}", adj_auto, adj_skill, adj_burst));

        annotator::finalize_success(&serde_json::to_string_pretty(&character).unwrap_or_default());

        let meta = ScanMeta {
            talent_suspicious,
            raw_skill: skill,
            raw_burst: burst,
        };

        CharacterResult::Scanned {
            viewed_index,
            character: Some(character),
            meta,
        }
    }

    /// Process Phase 2 rescan captures on the worker thread.
    ///
    /// Uses OCR constellation fallback as truth and click talent fallback as truth.
    fn process_rescan_captures(
        &self,
        captures: RescanCaptures,
        ocr_pool: &OcrPool,
        scaler: &CoordScaler,
    ) -> CharacterResult {
        let RescanCaptures {
            char_index,
            name,
            old,
            attrs_image,
            constellation_tab_image,
            constellation_node_images,
            talent_overview_image,
            talent_detail_images,
        } = captures;

        let ocr = ocr_pool.get();

        // -- Level --
        let (new_level, new_ascended, _) = Self::read_level_from_image(&ocr, &attrs_image, scaler);
        let new_ascension = level_to_ascension(new_level, new_ascended);

        // -- Constellation: pixel (primary) --
        let pixel_result = crate::scanner::common::pixel_utils::detect_constellation_pixel(
            &constellation_tab_image, scaler,
        );

        // -- Constellation: OCR fallback (always run, used as truth) --
        let skip_constellation = NO_CONSTELLATION_CHARACTERS.contains(&name.as_str());
        let new_constellation = if skip_constellation {
            0
        } else {
            let mut ocr_count = 0;
            for (i, node_image) in constellation_node_images.iter().enumerate() {
                let activated = Self::check_activation_from_image(&ocr, node_image, scaler);
                let field = format!("rescan_c{}", i + 1);
                annotator::record_ocr(&field, CHAR_CONSTELLATION_ACTIVATE_RECT, "");
                annotator::set_final(&field, if activated { "activated" } else { "locked" });
                if activated {
                    ocr_count = i as i32 + 1;
                } else {
                    break;
                }
            }

            if pixel_result.level != ocr_count {
                log_info!(
                    "[constellation] 验证 {}: 像素=C{} OCR=C{} → 使用OCR结果",
                    "[constellation] verify {}: pixel=C{} OCR=C{} → using OCR result",
                    name, pixel_result.level, ocr_count
                );
            }
            ocr_count
        };

        // -- Talents: overview (primary) --
        drop(ocr);
        let ((ov_auto, ov_skill, ov_burst), _) =
            Self::read_talents_from_image(ocr_pool, &talent_overview_image, &name, scaler);

        // -- Talents: click detail fallback (always run, used as truth) --
        let ocr = ocr_pool.get();
        let talent_labels = ["talent_detail_auto", "talent_detail_skill", "talent_detail_burst"];
        let mut det_levels = [0i32; 3];
        for (i, (img, label)) in talent_detail_images.iter().zip(talent_labels.iter()).enumerate() {
            annotator::add_image(label, img);
            det_levels[i] = Self::read_talent_from_detail_image(&ocr, img, scaler);
            annotator::record_ocr("talent_lv", CHAR_TALENT_LEVEL_RECT, "");
            annotator::set_final("talent_lv", &det_levels[i].to_string());
        }
        let [det_auto, det_skill, det_burst] = det_levels;
        log_debug!("[talent] 详情: 普攻={} 战技={} 爆发={}", "[talent] detail: auto={} skill={} burst={}",
            det_auto, det_skill, det_burst);

        // Use click fallback as truth when available, overview as fallback
        let raw_auto = if det_auto > 0 { det_auto } else if ov_auto > 0 { ov_auto } else { 1 };
        let raw_skill = if det_skill > 0 { det_skill } else if ov_skill > 0 { ov_skill } else { 1 };
        let raw_burst = if det_burst > 0 { det_burst } else if ov_burst > 0 { ov_burst } else { 1 };

        if det_auto > 0 && ov_auto > 0 && det_auto != ov_auto {
            log_debug!(
                "[talent] 验证 {} auto: 概览={} 详情={} → 使用详情",
                "[talent] verify {} auto: overview={} detail={} → using detail",
                name, ov_auto, det_auto
            );
        }
        if det_skill > 0 && ov_skill > 0 && det_skill != ov_skill {
            log_debug!(
                "[talent] 验证 {} skill: 概览={} 详情={} → 使用详情",
                "[talent] verify {} skill: overview={} detail={} → using detail",
                name, ov_skill, det_skill
            );
        }
        if det_burst > 0 && ov_burst > 0 && det_burst != ov_burst {
            log_debug!(
                "[talent] 验证 {} burst: 概览={} 详情={} → 使用详情",
                "[talent] verify {} burst: overview={} detail={} → using detail",
                name, ov_burst, det_burst
            );
        }

        // -- Build character with new data --
        let (adj_auto, adj_skill, adj_burst, _) =
            self.adjust_talents(raw_auto, raw_skill, raw_burst, &name, new_constellation);

        let good_element = if Self::ELEMENT_CHARACTERS.contains(&name.as_str()) {
            old.element.clone()
        } else {
            None
        };

        // Decide which values to use: prefer new if improved
        let level_improved = new_level > old.level;
        let constellation_changed = new_constellation != old.constellation;
        let old_talent_ones = [old.talent.auto, old.talent.skill, old.talent.burst]
            .iter().filter(|&&v| v == 1).count();
        let new_talent_ones = [adj_auto, adj_skill, adj_burst]
            .iter().filter(|&&v| v == 1).count();
        let talents_improved = new_talent_ones < old_talent_ones;

        let mut character = old.clone();
        if level_improved {
            character.level = new_level;
            character.ascension = new_ascension;
        }
        if constellation_changed {
            character.constellation = new_constellation;
        }
        if talents_improved || constellation_changed {
            character.talent.auto = adj_auto;
            character.talent.skill = adj_skill;
            character.talent.burst = adj_burst;
        }
        character.element = good_element;

        if level_improved || constellation_changed || talents_improved {
            let mut changes = Vec::new();
            if level_improved {
                changes.push(format!("Lv.{}→{}", old.level, new_level));
            }
            if constellation_changed {
                changes.push(format!("C{}→{}", old.constellation, new_constellation));
            }
            if talents_improved || constellation_changed {
                changes.push(format!("{}/{}/{}→{}/{}/{}",
                    old.talent.auto, old.talent.skill, old.talent.burst,
                    adj_auto, adj_skill, adj_burst));
            }
            log_info!(
                "[character] 验证 {}: {}",
                "[character] verify {}: {}",
                name, changes.join(", ")
            );
        } else {
            log_debug!(
                "[character] 验证 {}: 无变化",
                "[character] verify {}: no change",
                name
            );
        }

        CharacterResult::Rescanned {
            char_index,
            character,
        }
    }
}

// ── Main scan method ─────────────────────────────────────────────────────────

impl GoodCharacterScanner {
    /// Scan all characters using a two-phase controller/worker split.
    ///
    /// **Phase 1 (fast scan)**: Main thread captures images via deterministic
    /// tab sequence (attrs → constellation → talents → attrs); worker thread
    /// processes with OCR + pixel detection (no fallbacks).
    ///
    /// **Phase 2 (rescan)**: For suspicious characters only, main thread captures
    /// all fallback images (6 constellation nodes + 3 talent details); worker
    /// processes with OCR fallback as truth.
    pub fn scan(
        &self,
        ctrl: &mut GenshinGameController,
        start_at_char: usize,
        pools: &SharedOcrPools,
    ) -> Result<Vec<GoodCharacter>> {
        log_debug!("[character] 开始扫描...", "[character] starting scan...");
        let now = SystemTime::now();

        let ocr_pool = pools.v4().clone();
        // Hold one v5 instance on main thread for name OCR.
        let name_v5_guard = pools.v5().get();
        // Hold one v4 instance on main thread for name OCR fallback.
        let name_v4_guard = ocr_pool.get();

        // Return to main world and open character screen.
        ctrl.focus_game_window();
        if ctrl.check_rmb() { bail!("cancelled"); }
        ctrl.return_to_main_ui(8);
        if ctrl.check_rmb() { bail!("cancelled"); }

        let mut screen_opened = false;
        for attempt in 0..3 {
            if ctrl.check_rmb() { bail!("cancelled"); }
            ctrl.key_press(enigo::Key::Layout('c'));
            utils::sleep(self.config.open_delay as u32);

            let check_image = ctrl.capture_game()?;
            let (check_name, _, _) = self.read_name_from_image(
                Some(&name_v5_guard as &dyn ImageToText<RgbImage>),
                &name_v4_guard as &dyn ImageToText<RgbImage>,
                &check_image, &ctrl.scaler,
            );
            if check_name.is_some() {
                log_debug!("[character] 角色界面已检测到，第{}次尝试", "[character] character screen detected on attempt {}", attempt + 1);
                screen_opened = true;
                break;
            }

            log_debug!("[character] 未检测到角色界面（第{}次尝试），重试中...", "[character] character screen not detected (attempt {}), retrying...", attempt + 1);
            ctrl.return_to_main_ui(4);
        }
        if !screen_opened {
            log_error!("[character] 3次尝试后仍无法打开角色界面", "[character] failed to open character screen after 3 attempts");
            log_info!("{}", "{}", DELAY_TIP);
        }

        // Jump to the specified character index
        if start_at_char > 0 {
            log_debug!("[character] 跳转到角色索引 {}...", "[character] jumping to character index {}...", start_at_char);
            for _ in 0..start_at_char {
                ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
                utils::sleep((self.config.next_delay / 2).max(100) as u32);
            }
            utils::sleep(self.config.next_delay as u32);
        }

        // -- Spawn worker thread --
        let (work_tx, work_rx) = crossbeam_channel::bounded::<CharacterWork>(8);
        let (result_tx, result_rx) = crossbeam_channel::unbounded::<CharacterResult>();
        let worker_ocr_pool = ocr_pool.clone();
        let worker_mappings = self.mappings.clone();
        let worker_scaler = ctrl.scaler.clone();
        let worker_config = self.config.clone();

        let worker_handle = std::thread::spawn(move || {
            // Build a temporary scanner for processing (needs mappings for adjust_talents).
            let scanner = GoodCharacterScanner {
                config: worker_config,
                mappings: worker_mappings,
            };
            for work in work_rx {
                match work {
                    CharacterWork::Scan(captures) => {
                        let result = scanner.process_scan_captures(captures, &worker_ocr_pool, &worker_scaler);
                        let _ = result_tx.send(result);
                    }
                    CharacterWork::Rescan(captures) => {
                        let result = scanner.process_rescan_captures(captures, &worker_ocr_pool, &worker_scaler);
                        let _ = result_tx.send(result);
                    }
                    CharacterWork::Done => {
                        let _ = result_tx.send(CharacterResult::PhaseDone);
                    }
                }
            }
        });

        // ── Phase 1: Fast scan ──────────────────────────────────────────────

        let mut first_name: Option<String> = None;
        let mut viewed_count: usize = 0;
        let mut sent_count: usize = 0;
        let mut consecutive_failures: usize = 0;
        let mut reverse = false;

        // Parallel tracking: characters/metas/viewed_indices are filled as results arrive.
        let mut characters: Vec<GoodCharacter> = Vec::new();
        let mut scan_metas: Vec<ScanMeta> = Vec::new();
        let mut viewed_indices: Vec<usize> = Vec::new();

        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} [{elapsed_precise}] {msg}")
                .unwrap(),
        );
        pb.set_message("0 characters scanned");

        loop {
            if ctrl.check_rmb() {
                log_info!("[character] 用户中断扫描", "[character] user interrupted scan");
                break;
            }

            // Capture the current tab's image and OCR name.
            // The character name/element header is visible on ALL tabs,
            // so we can OCR it from whichever image we capture first.
            let first_image = ctrl.capture_game()?;
            let (name, element, raw_text) = self.read_name_from_image(
                Some(&name_v5_guard as &dyn ImageToText<RgbImage>),
                &name_v4_guard as &dyn ImageToText<RgbImage>,
                &first_image, &ctrl.scaler,
            );

            // Retry once on name failure
            let (name, element, raw_text, first_image) = if name.is_none() {
                log_debug!("[character] 首次名字匹配失败: \u{300C}{}\u{300D}，重试中...",
                    "[character] first name match failed: \u{300C}{}\u{300D}, retrying...", raw_text);
                utils::sleep(1000);
                let retry_image = ctrl.capture_game()?;
                let (n2, e2, r2) = self.read_name_from_image(
                    Some(&name_v5_guard as &dyn ImageToText<RgbImage>),
                    &name_v4_guard as &dyn ImageToText<RgbImage>,
                    &retry_image, &ctrl.scaler,
                );
                (n2, e2, r2, retry_image)
            } else {
                (name, element, raw_text, first_image)
            };

            let name = match name {
                Some(n) => n,
                None => {
                    if self.config.continue_on_failure {
                        log_warn!("[character] 无法识别: \u{300C}{}\u{300D}，跳过",
                            "[character] cannot identify: \u{300C}{}\u{300D}, skipping", raw_text);
                        log_info!("{}", "{}", DELAY_TIP);
                        consecutive_failures += 1;
                        viewed_count += 1;
                        // On name failure, return to attrs tab before navigating
                        // (we might be on either tab depending on `reverse`).
                        if reverse {
                            // Reverse starts on talents — go back to attrs
                            ctrl.click_at(CHAR_TAB_ATTRIBUTES.0, CHAR_TAB_ATTRIBUTES.1);
                            utils::sleep((self.config.tab_delay / 2) as u32);
                        }
                        ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
                        utils::sleep(self.config.next_delay as u32);
                        // Don't alternate on failure — stay with the same direction
                        // so the next character starts on a known tab (attrs).
                        reverse = false;
                        Self::drain_phase1_results(
                            &result_rx, &mut characters, &mut scan_metas,
                            &mut viewed_indices, &pb, self.config.log_progress,
                        );
                        if viewed_count > 3 && characters.is_empty() {
                            log_error!("[character] 已查看{}个但无结果，停止", "[character] viewed {} but no results, stopping", viewed_count);
                            log_info!("{}", "{}", DELAY_TIP);
                            break;
                        }
                        if consecutive_failures >= 5 {
                            log_error!("[character] 连续{}次失败，停止扫描", "[character] {} consecutive failures, stopping scan", consecutive_failures);
                            log_info!("{}", "{}", DELAY_TIP);
                            break;
                        }
                        continue;
                    }
                    bail!("无法识别角色 / Cannot identify character: \u{300C}{}\u{300D}\n{}", raw_text, DELAY_TIP);
                }
            };

            // Loop detection
            if let Some(ref first) = first_name {
                if &name == first {
                    log_info!("[character] 检测到循环，扫描完成", "[character] loop detected, scan complete");
                    break;
                }
            }
            if first_name.is_none() {
                first_name = Some(name.clone());
            }

            consecutive_failures = 0;
            let skip_constellation = NO_CONSTELLATION_CHARACTERS.contains(&name.as_str());

            // Capture all 3 images using alternating direction to save one tab switch.
            //
            // Forward (on attrs tab): attrs → constellation → talents → [end on talents]
            // Reverse (on talents tab): talents → constellation → attrs → [end on attrs]
            //
            // The first_image is from whichever tab we started on.
            let (attrs_image, constellation_image, talents_image);

            if !reverse {
                // Forward: first_image is the attrs capture
                attrs_image = first_image;

                constellation_image = if skip_constellation {
                    None
                } else {
                    ctrl.click_at(CHAR_TAB_CONSTELLATION.0, CHAR_TAB_CONSTELLATION.1);
                    utils::sleep(self.config.tab_delay as u32);
                    Some(ctrl.capture_game()?)
                };

                ctrl.click_at(CHAR_TAB_TALENTS.0, CHAR_TAB_TALENTS.1);
                utils::sleep(self.config.tab_delay as u32);
                talents_image = ctrl.capture_game()?;
                // End on talents tab → next iteration will be reverse
            } else {
                // Reverse: first_image is the talents capture
                talents_image = first_image;

                constellation_image = if skip_constellation {
                    None
                } else {
                    ctrl.click_at(CHAR_TAB_CONSTELLATION.0, CHAR_TAB_CONSTELLATION.1);
                    utils::sleep(self.config.tab_delay as u32);
                    Some(ctrl.capture_game()?)
                };

                ctrl.click_at(CHAR_TAB_ATTRIBUTES.0, CHAR_TAB_ATTRIBUTES.1);
                utils::sleep(self.config.tab_delay as u32);
                attrs_image = ctrl.capture_game()?;
                // End on attrs tab → next iteration will be forward
            }

            // Send to worker
            let captures = ScanCaptures {
                viewed_index: viewed_count,
                name,
                element,
                raw_name_text: raw_text,
                attrs_image,
                constellation_image,
                talents_image,
            };
            if work_tx.send(CharacterWork::Scan(captures)).is_err() {
                log_error!("[character] 工作通道已关闭", "[character] worker channel closed");
                break;
            }
            sent_count += 1;

            // Navigate to next character
            ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
            utils::sleep(self.config.next_delay as u32);
            viewed_count += 1;
            reverse = !reverse;

            // Drain available results (non-blocking)
            Self::drain_phase1_results(
                &result_rx, &mut characters, &mut scan_metas,
                &mut viewed_indices, &pb, self.config.log_progress,
            );

            // Check limits
            if self.config.max_count > 0 && characters.len() >= self.config.max_count {
                log_info!("[character] 已达到最大数量={}，停止", "[character] reached max_count={}, stopping", self.config.max_count);
                break;
            }
            if viewed_count > 3 && characters.is_empty() && sent_count > 3 {
                Self::drain_phase1_results(
                    &result_rx, &mut characters, &mut scan_metas,
                    &mut viewed_indices, &pb, self.config.log_progress,
                );
                if characters.is_empty() {
                    log_error!("[character] 已查看{}个但无结果，停止", "[character] viewed {} but no results, stopping", viewed_count);
                    log_info!("{}", "{}", DELAY_TIP);
                    break;
                }
            }
            if consecutive_failures >= 5 {
                log_error!("[character] 连续{}次失败，停止扫描", "[character] {} consecutive failures, stopping scan", consecutive_failures);
                log_info!("{}", "{}", DELAY_TIP);
                break;
            }
        }

        // Signal Phase 1 done and collect remaining results
        let _ = work_tx.send(CharacterWork::Done);
        loop {
            match result_rx.recv() {
                Ok(CharacterResult::Scanned { viewed_index, character, meta }) => {
                    if let Some(c) = character {
                        if self.config.log_progress {
                            log_debug!("[character] {} Lv.{} C{} {}/{}/{}",
                                "[character] {} Lv.{} C{} {}/{}/{}",
                                c.key, c.level, c.constellation,
                                c.talent.auto, c.talent.skill, c.talent.burst);
                        }
                        let msg = format!("{} Lv.{} C{}", c.key, c.level, c.constellation);
                        pb.set_message(format!("{} scanned — {}", characters.len() + 1, msg));
                        pb.tick();
                        characters.push(c);
                        viewed_indices.push(viewed_index);
                    }
                    scan_metas.push(meta);
                }
                Ok(CharacterResult::PhaseDone) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }

        pb.finish_with_message(format!("{} characters scanned", characters.len()));

        // Close character screen
        ctrl.key_press(enigo::Key::Escape);
        utils::sleep(self.config.close_delay as u32);

        // ── Phase 2: Rescan suspicious characters ───────────────────────────

        let suspicious: Vec<(usize, usize)> = characters.iter().enumerate()
            .filter(|(i, c)| {
                let meta = scan_metas.get(*i);
                Self::is_character_suspicious(c, meta)
            })
            .map(|(i, c)| {
                log_debug!(
                    "[character] 将重新读取 #{}: {} Lv.{} A{} C{} {}/{}/{}",
                    "[character] will re-read #{}: {} Lv.{} A{} C{} {}/{}/{}",
                    i, c.key, c.level, c.ascension, c.constellation,
                    c.talent.auto, c.talent.skill, c.talent.burst
                );
                (i, viewed_indices[i])
            })
            .collect();

        if !suspicious.is_empty() && !ctrl.is_cancelled() {
            log_info!(
                "[character] 第二轮: 重新读取{}个角色以提高精度",
                "[character] second pass: re-reading {} characters for accuracy",
                suspicious.len()
            );
            self.run_phase2(ctrl, &name_v4_guard as &dyn ImageToText<RgbImage>, &work_tx, &result_rx, &mut characters, &suspicious);
        }

        // Signal worker to exit
        drop(work_tx);
        let _ = worker_handle.join();

        // Final sanitize: snap impossible levels
        let mut had_impossible_level = false;
        for c in &mut characters {
            if (91..=94).contains(&c.level) {
                log_warn!("[character] {} 最终修正: {} → 90 (不可能的等级)", "[character] {} final snap: {} → 90 (impossible level)", c.key, c.level);
                c.level = 90;
                c.ascension = level_to_ascension(90, false);
                had_impossible_level = true;
            } else if (96..=99).contains(&c.level) {
                log_warn!("[character] {} 最终修正: {} → 95 (不可能的等级)", "[character] {} final snap: {} → 95 (impossible level)", c.key, c.level);
                c.level = 95;
                c.ascension = level_to_ascension(95, false);
                had_impossible_level = true;
            }
        }
        if had_impossible_level {
            log_info!("{}", "{}", DELAY_TIP);
        }

        let elapsed = now.elapsed().unwrap_or_default().as_secs_f64();
        if ctrl.is_cancelled() {
            log_info!(
                "[character] 已中断，扫描了{}个角色，耗时{:.3}s",
                "[character] interrupted, {} characters scanned in {:.3}s",
                characters.len(),
                elapsed
            );
        } else {
            log_info!(
                "[character] 完成，扫描了{}个角色，耗时{:.3}s",
                "[character] complete, {} characters scanned in {:.3}s",
                characters.len(),
                elapsed
            );
        }

        Ok(characters)
    }

    /// Non-blocking drain of Phase 1 results from the worker.
    fn drain_phase1_results(
        result_rx: &crossbeam_channel::Receiver<CharacterResult>,
        characters: &mut Vec<GoodCharacter>,
        scan_metas: &mut Vec<ScanMeta>,
        viewed_indices: &mut Vec<usize>,
        pb: &ProgressBar,
        log_progress: bool,
    ) {
        while let Ok(result) = result_rx.try_recv() {
            match result {
                CharacterResult::Scanned { viewed_index, character, meta } => {
                    if let Some(c) = character {
                        if log_progress {
                            log_debug!("[character] {} Lv.{} C{} {}/{}/{}",
                                "[character] {} Lv.{} C{} {}/{}/{}",
                                c.key, c.level, c.constellation,
                                c.talent.auto, c.talent.skill, c.talent.burst);
                        }
                        let msg = format!("{} Lv.{} C{}", c.key, c.level, c.constellation);
                        pb.set_message(format!("{} scanned — {}", characters.len() + 1, msg));
                        pb.tick();
                        characters.push(c);
                        viewed_indices.push(viewed_index);
                    }
                    scan_metas.push(meta);
                }
                _ => {} // Ignore unexpected messages during drain
            }
        }
    }

    /// Phase 2: reopen character screen, navigate to each suspicious character,
    /// capture all fallback images, and send to worker for processing.
    #[allow(unused_assignments)]
    fn run_phase2(
        &self,
        ctrl: &mut GenshinGameController,
        ocr: &dyn ImageToText<RgbImage>,
        work_tx: &crossbeam_channel::Sender<CharacterWork>,
        result_rx: &crossbeam_channel::Receiver<CharacterResult>,
        characters: &mut Vec<GoodCharacter>,
        suspicious: &[(usize, usize)], // (char_index, viewed_index)
    ) {
        // Return to main world and reopen character screen
        ctrl.return_to_main_ui(4);
        let mut screen_opened = false;
        for _attempt in 0..3 {
            ctrl.key_press(enigo::Key::Layout('c'));
            utils::sleep(self.config.open_delay as u32);
            if let Ok(img) = ctrl.capture_game() {
                let text = Self::ocr_image_region(ocr, &img, CHAR_NAME_RECT, &ctrl.scaler)
                    .unwrap_or_default();
                if !text.trim().is_empty() {
                    screen_opened = true;
                    break;
                }
            }
            ctrl.return_to_main_ui(4);
        }
        if !screen_opened {
            log_warn!("[character] 第二轮: 无法打开角色界面，跳过", "[character] second pass: failed to open character screen, skipping");
            return;
        }

        let mut current_index: usize = 0;
        let td = self.config.tab_delay;

        for &(char_idx, viewed_idx) in suspicious {
            if ctrl.check_rmb() {
                log_info!("[character] 第二轮: 用户中断", "[character] second pass: user interrupted");
                break;
            }

            // Navigate to target character
            let steps = if viewed_idx >= current_index {
                viewed_idx - current_index
            } else {
                // Wrap around: close and reopen to reset to 0
                ctrl.key_press(enigo::Key::Escape);
                utils::sleep(self.config.close_delay as u32);
                ctrl.return_to_main_ui(4);
                ctrl.key_press(enigo::Key::Layout('c'));
                utils::sleep(self.config.open_delay as u32);
                current_index = 0;
                viewed_idx
            };

            for _ in 0..steps {
                ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
                utils::sleep((self.config.next_delay / 2).max(100) as u32);
            }
            if steps > 0 {
                utils::sleep(self.config.next_delay as u32);
            }
            current_index = viewed_idx;

            let old = characters[char_idx].clone();
            let name = old.key.clone();
            let skip_constellation = NO_CONSTELLATION_CHARACTERS.contains(&name.as_str());

            // 1. Capture attributes
            let attrs_image = match ctrl.capture_game() {
                Ok(img) => img,
                Err(e) => {
                    log_error!("[character] 第二轮: 截图失败 {}: {}", "[character] second pass: capture failed for {}: {}", name, e);
                    continue;
                }
            };

            // 2. Constellation tab capture
            ctrl.click_at(CHAR_TAB_CONSTELLATION.0, CHAR_TAB_CONSTELLATION.1);
            utils::sleep(td as u32);
            let constellation_tab_image = ctrl.capture_game().unwrap_or_else(|_| RgbImage::new(1, 1));

            // 3. Click each constellation node and capture (6 captures)
            let mut constellation_node_images = Vec::with_capacity(6);
            if !skip_constellation {
                for ci in 0..6 {
                    let click_y = CHAR_CONSTELLATION_Y_BASE + ci as f64 * CHAR_CONSTELLATION_Y_STEP;
                    ctrl.click_at(CHAR_CONSTELLATION_X, click_y);
                    let delay = if ci == 0 { td * 3 / 4 } else { td / 2 };
                    utils::sleep(delay as u32);
                    constellation_node_images.push(
                        ctrl.capture_game().unwrap_or_else(|_| RgbImage::new(1, 1))
                    );
                }
                // Dismiss constellation popup
                ctrl.key_press(enigo::Key::Escape);
                utils::sleep(td as u32);
            }

            // 4. Talents overview capture
            ctrl.click_at(CHAR_TAB_TALENTS.0, CHAR_TAB_TALENTS.1);
            utils::sleep(td as u32);
            let talent_overview_image = ctrl.capture_game().unwrap_or_else(|_| RgbImage::new(1, 1));

            // 5. Click each talent detail and capture (3 captures)
            let has_special = SPECIAL_BURST_CHARACTERS.contains(&name.as_str());
            let talent_indices: [usize; 3] = if has_special { [0, 1, 3] } else { [0, 1, 2] };
            let mut talent_detail_images = Vec::with_capacity(3);
            for (ti, &talent_idx) in talent_indices.iter().enumerate() {
                let click_y = CHAR_TALENT_FIRST_Y + talent_idx as f64 * CHAR_TALENT_OFFSET_Y;
                ctrl.click_at(CHAR_TALENT_CLICK_X, click_y);
                let delay = if ti == 0 { td * 3 / 4 } else { td / 2 };
                utils::sleep(delay as u32);
                talent_detail_images.push(
                    ctrl.capture_game().unwrap_or_else(|_| RgbImage::new(1, 1))
                );
            }

            // 6. Dismiss talent detail popup and return to attributes
            ctrl.key_press(enigo::Key::Escape);
            utils::sleep(td as u32);
            ctrl.click_at(CHAR_TAB_ATTRIBUTES.0, CHAR_TAB_ATTRIBUTES.1);
            utils::sleep((td / 2) as u32);

            // 7. Send to worker
            let rescan = RescanCaptures {
                char_index: char_idx,
                name,
                old,
                attrs_image,
                constellation_tab_image,
                constellation_node_images,
                talent_overview_image,
                talent_detail_images,
            };
            if work_tx.send(CharacterWork::Rescan(rescan)).is_err() {
                log_error!("[character] 第二轮: 工作通道已关闭", "[character] second pass: worker channel closed");
                break;
            }
        }

        // Signal Phase 2 done and collect results
        let _ = work_tx.send(CharacterWork::Done);
        loop {
            match result_rx.recv() {
                Ok(CharacterResult::Rescanned { char_index, character }) => {
                    characters[char_index] = character;
                }
                Ok(CharacterResult::PhaseDone) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }

        // Close character screen
        ctrl.key_press(enigo::Key::Escape);
        utils::sleep(self.config.close_delay as u32);
    }
}

// ── Debug scan ───────────────────────────────────────────────────────────────

impl GoodCharacterScanner {
    /// Debug scan the currently displayed character.
    ///
    /// Uses the same capture-then-process pattern as the main scanner.
    /// The character screen must already be open and showing a character.
    pub fn debug_scan_current(
        &self,
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &mut GenshinGameController,
    ) -> DebugScanResult {
        use std::time::Instant;

        let total_start = Instant::now();
        let mut fields = Vec::new();
        let scaler = ctrl.scaler.clone();

        // Capture attributes and OCR name + level
        let t = Instant::now();
        let attrs_image = ctrl.capture_game().unwrap_or_else(|_| RgbImage::new(1, 1));
        let (name, element, raw_text) = self.read_name_from_image(None, ocr, &attrs_image, &scaler);
        let name_key = name.unwrap_or_default();
        fields.push(DebugOcrField {
            field_name: "name".into(),
            raw_text: raw_text,
            parsed_value: format!("{} ({})", name_key, element.as_deref().unwrap_or("?")),
            region: CHAR_NAME_RECT,
            duration_ms: t.elapsed().as_millis() as u64,
        });

        let t = Instant::now();
        let (level, ascended, _) = Self::read_level_from_image(ocr, &attrs_image, &scaler);
        let ascension = level_to_ascension(level, ascended);
        fields.push(DebugOcrField {
            field_name: "level".into(),
            raw_text: String::new(),
            parsed_value: format!("lv={} ascended={} asc={}", level, ascended, ascension),
            region: CHAR_LEVEL_RECT,
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Constellation: click tab, capture, pixel detect
        let t = Instant::now();
        let constellation = if NO_CONSTELLATION_CHARACTERS.contains(&name_key.as_str()) {
            0
        } else {
            ctrl.click_at(CHAR_TAB_CONSTELLATION.0, CHAR_TAB_CONSTELLATION.1);
            utils::sleep(self.config.tab_delay as u32);
            let const_image = ctrl.capture_game().unwrap_or_else(|_| RgbImage::new(1, 1));
            let result = crate::scanner::common::pixel_utils::detect_constellation_pixel(
                &const_image, &scaler,
            );
            result.level
        };
        fields.push(DebugOcrField {
            field_name: "constellation".into(),
            raw_text: String::new(),
            parsed_value: format!("C{}", constellation),
            region: (0.0, 0.0, 0.0, 0.0),
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Talents: click tab, capture, OCR overview
        let t = Instant::now();
        ctrl.click_at(CHAR_TAB_TALENTS.0, CHAR_TAB_TALENTS.1);
        utils::sleep(self.config.tab_delay as u32);
        let talents_image = ctrl.capture_game().unwrap_or_else(|_| RgbImage::new(1, 1));
        let ocr_backend = self.config.ocr_backend.clone();
        let debug_pool = OcrPool::new(
            move || ocr_factory::create_ocr_model(&ocr_backend),
            3,
        ).ok();
        let (auto, skill, burst) = if let Some(ref pool) = debug_pool {
            let ((a, s, b), _) = Self::read_talents_from_image(pool, &talents_image, &name_key, &scaler);
            (if a > 0 { a } else { 1 }, if s > 0 { s } else { 1 }, if b > 0 { b } else { 1 })
        } else {
            (1, 1, 1)
        };
        fields.push(DebugOcrField {
            field_name: "talents".into(),
            raw_text: String::new(),
            parsed_value: format!("{}/{}/{}", auto, skill, burst),
            region: (0.0, 0.0, 0.0, 0.0),
            duration_ms: t.elapsed().as_millis() as u64,
        });

        let good_element = if Self::ELEMENT_CHARACTERS.contains(&name_key.as_str()) {
            element.as_deref().and_then(Self::zh_element_to_good)
        } else {
            None
        };
        let character = GoodCharacter {
            key: name_key,
            level,
            constellation,
            ascension,
            talent: GoodTalent { auto, skill, burst },
            element: good_element,
        };
        let parsed_json = serde_json::to_string_pretty(&character).unwrap_or_default();

        DebugScanResult {
            fields,
            total_duration_ms: total_start.elapsed().as_millis() as u64,
            parsed_json,
        }
    }
}
