use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{bail, Result};
use image::{GenericImageView, RgbImage};
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, error, info, warn};
use regex::Regex;

use yas::ocr::ImageToText;
use yas::utils;

use super::GoodCharacterScannerConfig;
use crate::scanner::common::constants::*;
use crate::scanner::common::coord_scaler::CoordScaler;
use crate::scanner::common::debug_dump::DumpCtx;
use crate::scanner::common::fuzzy_match::fuzzy_match_map;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::MappingManager;
use crate::scanner::common::models::{DebugOcrField, DebugScanResult, GoodCharacter, GoodTalent};
use crate::scanner::common::ocr_factory;
use crate::scanner::common::ocr_pool::OcrPool;
use crate::scanner::common::stat_parser::level_to_ascension;

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

/// Character scanner ported from GOODScanner/lib/character_scanner.js.
///
/// Uses binary-search constellation detection (max 3 clicks),
/// talent adjustments (Tartaglia -1, C3/C5 bonus subtraction),
/// and alternating scan direction for tab optimization.
///
/// The scanner holds only business logic (OCR model, mappings, config).
/// The game controller is passed to `scan()` to share it across scanners.
pub struct GoodCharacterScanner {
    config: GoodCharacterScannerConfig,
    mappings: Arc<MappingManager>,
}

impl GoodCharacterScanner {
    pub fn new(
        config: GoodCharacterScannerConfig,
        mappings: Arc<MappingManager>,
    ) -> Result<Self> {
        Ok(Self {
            config,
            mappings,
        })
    }
}

impl GoodCharacterScanner {
    /// OCR a region in base 1920x1080 coordinates, capturing a fresh frame.
    fn ocr_rect(
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &GenshinGameController,
        rect: (f64, f64, f64, f64),
    ) -> Result<String> {
        ctrl.ocr_region(ocr, rect)
    }

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
    fn parse_name_and_element(&self, text: &str) -> (Option<String>, Option<String>) {
        if text.is_empty() {
            return (None, None);
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
            let name = fuzzy_match_map(&raw_name, &self.mappings.character_name_map);
            (name, Some(element))
        } else {
            let name = fuzzy_match_map(text, &self.mappings.character_name_map);
            (name, None)
        }
    }

    /// OCR read character name and element, with one retry.
    fn read_name_and_element(
        &self,
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &GenshinGameController,
    ) -> Result<(Option<String>, Option<String>, String)> {
        let text = Self::ocr_rect(ocr, ctrl, CHAR_NAME_RECT)?;
        let (name, element) = self.parse_name_and_element(&text);

        if name.is_some() {
            debug!("[character] name OCR: {:?} -> {:?}", text, name);
            return Ok((name, element, text));
        }

        warn!("[character] first name match failed: \u{300C}{}\u{300D}, retrying...", text);
        utils::sleep(1000);

        let text2 = Self::ocr_rect(ocr, ctrl, CHAR_NAME_RECT)?;
        let (name2, element2) = self.parse_name_and_element(&text2);
        if name2.is_none() {
            warn!("[character] second name match failed: \u{300C}{}\u{300D}", text2);
        }
        Ok((name2, element2, text2))
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
        // Snap max to nearest valid cap
        let max_level = Self::VALID_MAX_LEVELS
            .iter()
            .copied()
            .min_by_key(|&v| (v - max_level).unsigned_abs())
            .unwrap_or(max_level);
        // Levels 95 and 100 always equal their cap (no partial progress)
        let level = if max_level >= 95 { max_level } else { level.min(max_level) };
        let ascended = level >= 20 && level < max_level;
        (level, ascended)
    }

    /// Check if a (level, max) pair is plausible.
    /// Returns false if level > max, or level < minimum for that cap.
    fn is_level_plausible(level: i32, max_level: i32) -> bool {
        if level > max_level || level < 1 {
            return false;
        }
        // Find the minimum level for this cap
        if let Some(idx) = Self::VALID_MAX_LEVELS.iter().position(|&v| v == max_level) {
            let min_lv = Self::MIN_LEVEL_FOR_CAP[idx];
            level >= min_lv
        } else {
            // Unknown cap — can't validate, assume OK
            true
        }
    }

    /// Try to split a digit string into (level, max) pair.
    /// Returns Some((level, max)) if a valid split is found.
    fn try_split_digits(digits: &str) -> Option<(i32, i32)> {
        // Try splits from longest level first (prefer 90/90 over 9/090)
        for i in (1..digits.len()).rev() {
            if let (Ok(lv), Ok(mx)) = (digits[..i].parse::<i32>(), digits[i..].parse::<i32>()) {
                if (1..=100).contains(&lv) && (10..=100).contains(&mx) && mx >= lv {
                    return Some((lv, mx));
                }
            }
        }
        None
    }

    /// OCR read character level once, returns (level, ascended).
    fn read_level_once(
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &GenshinGameController,
    ) -> Result<(i32, bool)> {
        let text = Self::ocr_rect(ocr, ctrl, CHAR_LEVEL_RECT)?;

        // Try standard "XX/YY" format — tolerant of OCR noise (·, ., :, spaces) around the slash
        let re = Regex::new(r"(\d+)\s*[./·:]*\s*/\s*[./·:]*\s*(\d+)")?;
        if let Some(caps) = re.captures(&text) {
            let level: i32 = caps[1].parse().unwrap_or(1);
            let raw_max: i32 = caps[2].parse().unwrap_or(20);
            return Ok(Self::finalize_level(level, raw_max));
        }

        // Fallback: extract all digit characters and try to split into level/max pair
        let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let raw: i64 = digits.parse().unwrap_or(0);
            if raw > 0 && raw <= 100 {
                return Ok((raw as i32, false));
            }

            // Phase 1: clean split (e.g. "9090" → 90/90)
            if let Some((lv, mx)) = Self::try_split_digits(&digits) {
                debug!("[character] level OCR fallback split: {:?} -> {}/{}", digits, lv, mx);
                return Ok(Self::finalize_level(lv, mx));
            }

            // Phase 2: remove one noise char at each position and retry
            // OCR often turns "/" into a digit (e.g. "70180" = 70 + '1' + 80)
            // The noise char is between level and max digits, so prefer removing
            // from the middle of the string.
            {
                let mid = digits.len() as f64 / 2.0;
                let mut best_noise: Option<(i32, i32, usize, f64)> = None; // (level, max, idx, dist_from_mid)
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
                    warn!(
                        "[character] level OCR noise-remove split: {:?} (remove idx {}) -> {}/{}",
                        digits, idx, lv, mx
                    );
                    return Ok(Self::finalize_level(lv, mx));
                }
            }

            // Phase 3: take first 2-3 digits as level (no max info)
            for len in [3, 2] {
                if digits.len() >= len {
                    if let Ok(lv) = digits[..len].parse::<i32>() {
                        if (1..=100).contains(&lv) {
                            warn!("[character] level OCR partial extract: {:?} -> {}", digits, lv);
                            return Ok((lv, false));
                        }
                    }
                }
            }
        }

        warn!("[character] level OCR completely failed: {:?}", text);
        // Save the level region for debugging
        if let Ok(im) = ctrl.capture_region(CHAR_LEVEL_RECT.0, CHAR_LEVEL_RECT.1, CHAR_LEVEL_RECT.2, CHAR_LEVEL_RECT.3) {
            let ts = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
            let path = format!("debug_level_fail_{}.png", ts);
            let _ = im.save(&path);
            warn!("[character] saved failed level region to {}", path);
        }
        Ok((1, false))
    }

    /// Derive the effective max level (cap) from a level reading.
    fn derive_max_level(level: i32, ascended: bool) -> i32 {
        if ascended {
            // ascended means level < max, find the cap above level
            Self::VALID_MAX_LEVELS.iter().copied().find(|&v| v > level).unwrap_or(100)
        } else if Self::VALID_MAX_LEVELS.contains(&level) {
            // At cap exactly (not ascended) — max = level
            level
        } else {
            // Between caps, not ascended — find cap above
            Self::VALID_MAX_LEVELS.iter().copied().find(|&v| v > level).unwrap_or(100)
        }
    }

    /// Check if a level reading looks suspicious and warrants a retry.
    ///
    /// Suspicious cases:
    /// 1. Level is in an impossible range (91-94 or 96-99 don't exist)
    /// 2. Level is a single digit 2-9 (likely truncated — e.g., "90" → "9")
    /// 3. Level is implausible for its cap (below minimum for that ascension tier)
    fn is_level_suspicious(level: i32, ascended: bool) -> bool {
        // Impossible levels: 91-94 and 96-99 don't exist (jump 90→95→100)
        if (91..=94).contains(&level) || (96..=99).contains(&level) {
            return true;
        }

        // Single-digit levels are almost always OCR errors (truncated)
        // Exception: level 1 at cap 20 is valid for fresh characters
        if level >= 2 && level < 10 {
            return true;
        }

        // Check plausibility against derived cap
        let max_level = Self::derive_max_level(level, ascended);
        if !Self::is_level_plausible(level, max_level) {
            return true;
        }

        false
    }

    /// OCR read character level.
    ///
    /// Reads once and logs if the result looks suspicious. Suspicious results
    /// are handled by the second-pass rescan in `scan()` rather than immediate
    /// retry (since re-OCRing the same frame rarely yields different results).
    fn read_level(
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &GenshinGameController,
    ) -> Result<(i32, bool)> {
        let (level, ascended) = Self::read_level_once(ocr, ctrl)?;

        if Self::is_level_suspicious(level, ascended) {
            let max_level = Self::derive_max_level(level, ascended);
            warn!(
                "[character] level {} (max={}, ascended={}) looks suspicious, will rescan in second pass",
                level, max_level, ascended
            );
        }

        Ok((level, ascended))
    }

    /// Click a constellation node and check if it's activated via OCR.
    fn is_constellation_activated(
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &mut GenshinGameController,
        c_index: usize,
        is_first_click: bool,
        tab_delay: u64,
        dump: &Option<DumpCtx>,
    ) -> Result<bool> {
        let click_y = CHAR_CONSTELLATION_Y_BASE + c_index as f64 * CHAR_CONSTELLATION_Y_STEP;
        ctrl.click_at(CHAR_CONSTELLATION_X, click_y);

        let delay = if is_first_click { tab_delay * 3 / 4 } else { tab_delay / 2 };
        utils::sleep(delay as u32);

        // Dump per-constellation-node image
        if let Some(ref ctx) = dump {
            if let Ok(img) = ctrl.capture_game() {
                ctx.dump_region(
                    &format!("constellation_c{}", c_index + 1),
                    &img, CHAR_CONSTELLATION_ACTIVATE_RECT, &ctrl.scaler,
                );
            }
        }

        let text = Self::ocr_rect(ocr, ctrl, CHAR_CONSTELLATION_ACTIVATE_RECT)?;
        // "已激活" means "Activated"
        Ok(text.contains("\u{5DF2}\u{6FC0}\u{6D3B}"))
    }

    /// Debug: capture constellation page screenshot and sample pixel lightness.
    /// Saves full constellation page + logs lightness at each node position.
    #[allow(dead_code)]
    fn debug_constellation_lightness(
        ctrl: &GenshinGameController,
        character_name: &str,
    ) {
        let image = match ctrl.capture_game() {
            Ok(im) => im,
            Err(e) => {
                warn!("[constellation-debug] capture failed: {}", e);
                return;
            }
        };

        // Save the full constellation page
        let path = format!("debug_constellation_{}.png", character_name);
        let _ = image.save(&path);
        debug!("[constellation-debug] saved: {}", path);

        // Sample lightness at each constellation node position
        let scaler = &ctrl.scaler;
        for i in 0..6 {
            let base_y = CHAR_CONSTELLATION_Y_BASE + i as f64 * CHAR_CONSTELLATION_Y_STEP;
            let sx = scaler.x(CHAR_CONSTELLATION_X) as u32;
            let sy = scaler.y(base_y) as u32;

            if sx < image.width() && sy < image.height() {
                let pixel = image.get_pixel(sx, sy);
                let r = pixel[0] as f64;
                let g = pixel[1] as f64;
                let b = pixel[2] as f64;
                let lightness = (r + g + b) / 3.0;

                // Also sample a small 5x5 region around the point for average lightness
                let mut sum = 0.0;
                let mut count = 0;
                for dy in 0..5_i32 {
                    for dx in 0..5_i32 {
                        let px = (sx as i32 + dx - 2).max(0) as u32;
                        let py = (sy as i32 + dy - 2).max(0) as u32;
                        if px < image.width() && py < image.height() {
                            let p = image.get_pixel(px, py);
                            sum += (p[0] as f64 + p[1] as f64 + p[2] as f64) / 3.0;
                            count += 1;
                        }
                    }
                }
                let avg_lightness = if count > 0 { sum / count as f64 } else { 0.0 };

                debug!(
                    "[constellation-debug] {} C{}: pixel({},{}) = ({},{},{}) lightness={:.1} avg5x5={:.1}",
                    character_name, i + 1, sx, sy, pixel[0], pixel[1], pixel[2], lightness, avg_lightness
                );
            }
        }
    }

    /// Binary-search constellation count (max 3 clicks).
    fn read_constellation_count(
        &self,
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &mut GenshinGameController,
        character_name: &str,
        _element: &Option<String>,
        dump: &Option<DumpCtx>,
    ) -> Result<i32> {
        if NO_CONSTELLATION_CHARACTERS.contains(&character_name) {
            return Ok(0);
        }

        ctrl.click_at(CHAR_TAB_CONSTELLATION.0, CHAR_TAB_CONSTELLATION.1);
        utils::sleep(self.config.tab_delay as u32);

        let td = self.config.tab_delay;
        let constellation;

        let c3 = Self::is_constellation_activated(ocr, ctrl, 2, true, td, dump)?;
        if !c3 {
            let c2 = Self::is_constellation_activated(ocr, ctrl, 1, false, td, dump)?;
            if !c2 {
                let c1 = Self::is_constellation_activated(ocr, ctrl, 0, false, td, dump)?;
                constellation = if c1 { 1 } else { 0 };
            } else {
                constellation = 2;
            }
        } else {
            let c6 = Self::is_constellation_activated(ocr, ctrl, 5, false, td, dump)?;
            if c6 {
                constellation = 6;
            } else {
                let c4 = Self::is_constellation_activated(ocr, ctrl, 3, false, td, dump)?;
                if !c4 {
                    constellation = 3;
                } else {
                    let c5 = Self::is_constellation_activated(ocr, ctrl, 4, false, td, dump)?;
                    constellation = if c5 { 5 } else { 4 };
                }
            }
        }

        // Dump the full constellation screen BEFORE dismissing the popup
        if let Some(ref ctx) = dump {
            if let Ok(img) = ctrl.capture_game() {
                ctx.dump_region("constellation_screen", &img, (0.0, 0.0, 1920.0, 1080.0), &ctrl.scaler);
            }
        }

        // Dismiss the constellation detail popup with Escape.
        // Do NOT click_at(1600, 30) — that hits the mora display and can
        // trigger unintended navigation.
        ctrl.key_press(enigo::Key::Escape);
        utils::sleep(self.config.tab_delay as u32);

        Ok(constellation)
    }

    /// Parse "Lv.X" format from talent overview text.
    ///
    /// Tolerant of OCR errors: accepts Lv, LV, Ly, lv with ./:/ /no separator
    /// Port of `parseLvText()` from character_scanner.js
    fn parse_lv_text(text: &str) -> i32 {
        if text.is_empty() {
            return 0;
        }
        // Strip spaces between digits — OCR on small text can insert spaces ("1 1" → "11")
        let clean: String = {
            let chars: Vec<char> = text.chars().collect();
            let mut result = String::with_capacity(text.len());
            for (i, &c) in chars.iter().enumerate() {
                if c == ' ' && i > 0 && i + 1 < chars.len()
                    && chars[i - 1].is_ascii_digit() && chars[i + 1].is_ascii_digit()
                {
                    continue; // Skip space between digits
                }
                result.push(c);
            }
            result
        };
        // Accept various OCR corruptions: Lv, LV, Ly, lv, with . : or space separator
        let re = Regex::new(r"(?i)[Ll][VvYy][.:．]?\s*(\d{1,2})").unwrap();
        if let Some(caps) = re.captures(&clean) {
            let lv: i32 = caps[1].parse().unwrap_or(0);
            if (1..=15).contains(&lv) {
                return lv;
            }
        }
        // Broader fallback: just look for any 1-2 digit number
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

        // Tartaglia innate talent: auto +1 bonus
        if name == TARTAGLIA_KEY {
            auto = (auto - 1).max(1);
        }

        // Helper: subtract 3 from a talent, flagging if it's too low
        let sub3 = |val: &mut i32, sus: &mut bool| {
            if *val < 4 {
                *sus = true;
            }
            *val = (*val - 3).max(1);
        };

        // Subtract constellation talent bonuses (C3/C5 each add +3)
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
            // Traveler has element-specific constellations not in mappings.
            // Heuristic: C≥5 means both E and Q get +3 bonuses.
            // Otherwise, if E or Q reads >10 it likely has a +3 bonus.
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

    /// Read a single talent level by clicking the detail view.
    fn read_talent_by_click(
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &mut GenshinGameController,
        talent_index: usize,
        is_first: bool,
        tab_delay: u64,
    ) -> Result<i32> {
        let click_y = CHAR_TALENT_FIRST_Y + talent_index as f64 * CHAR_TALENT_OFFSET_Y;
        ctrl.click_at(CHAR_TALENT_CLICK_X, click_y);

        let delay = if is_first { tab_delay * 3 / 4 } else { tab_delay / 2 };
        utils::sleep(delay as u32);

        let text = Self::ocr_rect(ocr, ctrl, CHAR_TALENT_LEVEL_RECT)?;
        debug!("[talent] click fallback idx={} raw OCR: {:?}", talent_index, text);
        let re = Regex::new(r"[Ll][Vv]\.?\s*(\d{1,2})")?;
        if let Some(caps) = re.captures(&text) {
            let v: i32 = caps[1].parse().unwrap_or(1);
            if (1..=15).contains(&v) {
                return Ok(v);
            }
        }
        // Broader fallback: just find any 1-2 digit number
        let re2 = Regex::new(r"(\d{1,2})")?;
        if let Some(caps) = re2.captures(&text) {
            let v: i32 = caps[1].parse().unwrap_or(1);
            if (1..=15).contains(&v) {
                return Ok(v);
            }
        }
        warn!("[talent] click fallback failed for idx={}, defaulting to 1", talent_index);
        Ok(1)
    }

    /// Read all three talent levels using overview OCR first, with click fallback.
    ///
    /// Captures the talent overview screen once, then OCRs all 3 regions
    /// in parallel using rayon for ~3x faster talent reading.
    fn read_talent_levels(
        &self,
        ocr_pool: &OcrPool,
        ctrl: &mut GenshinGameController,
        character_name: &str,
        skip_tab: bool,
    ) -> Result<(i32, i32, i32)> {
        if !skip_tab {
            ctrl.click_at(CHAR_TAB_TALENTS.0, CHAR_TAB_TALENTS.1);
            utils::sleep(self.config.tab_delay as u32);
        }

        let has_special = SPECIAL_BURST_CHARACTERS.contains(&character_name);
        let burst_rect = if has_special {
            CHAR_TALENT_OVERVIEW_BURST_SPECIAL
        } else {
            CHAR_TALENT_OVERVIEW_BURST
        };

        // Capture once, OCR 3 regions in parallel
        let image = ctrl.capture_game()?;
        let scaler = ctrl.scaler.clone();

        let (auto_lv, (skill_lv, burst_lv)) = rayon::join(
            || {
                let ocr = ocr_pool.get();
                Self::ocr_image_region(&ocr, &image, CHAR_TALENT_OVERVIEW_AUTO, &scaler)
                    .map(|t| { let lv = Self::parse_lv_text(&t); debug!("[talent] overview auto: 「{}」 → {}", t.trim(), lv); lv })
                    .unwrap_or(0)
            },
            || {
                rayon::join(
                    || {
                        let ocr = ocr_pool.get();
                        Self::ocr_image_region(&ocr, &image, CHAR_TALENT_OVERVIEW_SKILL, &scaler)
                            .map(|t| { let lv = Self::parse_lv_text(&t); debug!("[talent] overview skill: 「{}」 → {}", t.trim(), lv); lv })
                            .unwrap_or(0)
                    },
                    || {
                        let ocr = ocr_pool.get();
                        Self::ocr_image_region(&ocr, &image, burst_rect, &scaler)
                            .map(|t| { let lv = Self::parse_lv_text(&t); debug!("[talent] overview burst: 「{}」 → {}", t.trim(), lv); lv })
                            .unwrap_or(0)
                    },
                )
            },
        );

        let mut auto = if auto_lv > 0 { auto_lv } else { 1 };
        let mut skill = if skill_lv > 0 { skill_lv } else { 1 };
        let mut burst = if burst_lv > 0 { burst_lv } else { 1 };

        // Fallback to click-detail for any that failed
        let need_click = auto_lv == 0 || skill_lv == 0 || burst_lv == 0;
        if need_click {
            let ocr_guard = ocr_pool.get();
            let mut missing = Vec::new();
            if auto_lv == 0 { missing.push("auto"); }
            if skill_lv == 0 { missing.push("skill"); }
            if burst_lv == 0 { missing.push("burst"); }
            warn!(
                "[character] talent overview failed for: {}, using click fallback",
                missing.join("/")
            );

            let td = self.config.tab_delay;
            let mut is_first = true;
            if auto_lv == 0 {
                auto = Self::read_talent_by_click(&ocr_guard, ctrl, 0, is_first, td)?;
                is_first = false;
            }
            if skill_lv == 0 {
                skill = Self::read_talent_by_click(&ocr_guard, ctrl, 1, is_first, td)?;
                is_first = false;
            }
            if burst_lv == 0 {
                let burst_index = if has_special { 3 } else { 2 };
                burst = Self::read_talent_by_click(&ocr_guard, ctrl, burst_index, is_first, td)?;
            }
            ctrl.key_press(enigo::Key::Escape);
            utils::sleep(td as u32);
        }

        Ok((auto, skill, burst))
    }

    /// Scan a single character.
    ///
    /// `first_name`: the first character's key for loop detection (None on first scan).
    /// `reverse`: if true, scan in talents→constellation→attributes order.
    ///
    /// Returns `Ok((Some(character), talent_suspicious))` on success,
    /// `Ok((None, false))` to skip, or error for loop detection / fatal.
    ///
    /// Port of `scanSingleCharacter()` from character_scanner.js
    fn scan_single_character(
        &self,
        ocr_pool: &OcrPool,
        ctrl: &mut GenshinGameController,
        first_name: &Option<String>,
        reverse: bool,
        char_index: usize,
    ) -> Result<(Option<GoodCharacter>, ScanMeta)> {
        let ocr = ocr_pool.get();

        // Name and element are visible from any tab
        let (name, element, raw_text) = self.read_name_and_element(&ocr, ctrl)?;

        let name = match name {
            Some(n) => n,
            None => {
                if self.config.continue_on_failure {
                    warn!("[character] cannot identify: \u{300C}{}\u{300D}, skipping", raw_text);
                    return Ok((None, ScanMeta { talent_suspicious: false, raw_skill: 0, raw_burst: 0 }));
                }
                bail!("无法识别角色 / Cannot identify character: \u{300C}{}\u{300D}", raw_text);
            }
        };

        // Loop detection
        if let Some(first) = first_name {
            if &name == first {
                return Err(anyhow::anyhow!("_repeat"));
            }
        }

        // Set up dump context if image dumping is enabled
        let dump = if self.config.dump_images {
            Some(DumpCtx::new("debug_images", "characters", char_index, &name))
        } else {
            None
        };

        let level_info;
        let constellation;
        let talents;

        if !reverse {
            // Forward: attributes → constellation → talents (already on attributes tab)

            // Dump the attributes screen (name + level visible)
            if let Some(ref ctx) = dump {
                if let Ok(img) = ctrl.capture_game() {
                    ctx.dump_full(&img);
                    ctx.dump_region("name", &img, CHAR_NAME_RECT, &ctrl.scaler);
                    ctx.dump_region("level", &img, CHAR_LEVEL_RECT, &ctrl.scaler);
                }
            }

            level_info = Self::read_level(&ocr, ctrl)?;
            constellation = self.read_constellation_count(&ocr, ctrl, &name, &element, &dump)?;

            // Drop the single OCR guard before talent reading (which uses pool internally)
            drop(ocr);
            talents = self.read_talent_levels(ocr_pool, ctrl, &name, false)?;

            // Dump the talent overview screen
            if let Some(ref ctx) = dump {
                if let Ok(img) = ctrl.capture_game() {
                    let has_special = SPECIAL_BURST_CHARACTERS.contains(&name.as_str());
                    let burst_rect = if has_special { CHAR_TALENT_OVERVIEW_BURST_SPECIAL } else { CHAR_TALENT_OVERVIEW_BURST };
                    ctx.dump_region("talent_screen", &img, (0.0, 0.0, 1920.0, 1080.0), &ctrl.scaler);
                    ctx.dump_region("talent_auto", &img, CHAR_TALENT_OVERVIEW_AUTO, &ctrl.scaler);
                    ctx.dump_region("talent_skill", &img, CHAR_TALENT_OVERVIEW_SKILL, &ctrl.scaler);
                    ctx.dump_region("talent_burst", &img, burst_rect, &ctrl.scaler);
                }
            }
        } else {
            // Reverse: talents → constellation → attributes (already on talents tab)
            // Drop the single OCR guard before talent reading (which uses pool internally)
            drop(ocr);
            talents = self.read_talent_levels(ocr_pool, ctrl, &name, true)?;

            // Dump the talent overview screen
            if let Some(ref ctx) = dump {
                if let Ok(img) = ctrl.capture_game() {
                    let has_special = SPECIAL_BURST_CHARACTERS.contains(&name.as_str());
                    let burst_rect = if has_special { CHAR_TALENT_OVERVIEW_BURST_SPECIAL } else { CHAR_TALENT_OVERVIEW_BURST };
                    ctx.dump_region("talent_screen", &img, (0.0, 0.0, 1920.0, 1080.0), &ctrl.scaler);
                    ctx.dump_region("talent_auto", &img, CHAR_TALENT_OVERVIEW_AUTO, &ctrl.scaler);
                    ctx.dump_region("talent_skill", &img, CHAR_TALENT_OVERVIEW_SKILL, &ctrl.scaler);
                    ctx.dump_region("talent_burst", &img, burst_rect, &ctrl.scaler);
                }
            }

            let ocr = ocr_pool.get();
            constellation = self.read_constellation_count(&ocr, ctrl, &name, &element, &dump)?;

            ctrl.click_at(CHAR_TAB_ATTRIBUTES.0, CHAR_TAB_ATTRIBUTES.1);
            utils::sleep(self.config.tab_delay as u32);
            level_info = Self::read_level(&ocr, ctrl)?;

            // Dump the attributes screen (name + level visible)
            if let Some(ref ctx) = dump {
                if let Ok(img) = ctrl.capture_game() {
                    ctx.dump_region("attributes_screen", &img, (0.0, 0.0, 1920.0, 1080.0), &ctrl.scaler);
                    ctx.dump_region("name", &img, CHAR_NAME_RECT, &ctrl.scaler);
                    ctx.dump_region("level", &img, CHAR_LEVEL_RECT, &ctrl.scaler);
                }
            }
        }

        let (level, ascended) = level_info;
        let ascension = level_to_ascension(level, ascended);

        let (auto, skill, burst, talent_suspicious) =
            self.adjust_talents(talents.0, talents.1, talents.2, &name, constellation);

        // Set element for multi-element characters
        let good_element = if Self::ELEMENT_CHARACTERS.contains(&name.as_str()) {
            element.as_deref().and_then(Self::zh_element_to_good)
        } else {
            None
        };

        Ok((Some(GoodCharacter {
            key: name,
            level,
            constellation,
            ascension,
            talent: GoodTalent {
                auto,
                skill,
                burst,
            },
            element: good_element,
        }), ScanMeta {
            talent_suspicious,
            raw_skill: talents.1,
            raw_burst: talents.2,
        }))
    }

    /// Scan all characters by iterating through the character list.
    ///
    /// Alternates scan direction (reverse flag) each character for tab optimization.
    /// Detects loop completion when the first character is seen again.
    ///
    /// Port of `scanAllCharacters()` from character_scanner.js
    /// Scan all characters.
    ///
    /// If `start_at_char > 0`, presses right arrow that many times to
    /// jump to a specific character index before scanning.
    pub fn scan(&self, ctrl: &mut GenshinGameController, start_at_char: usize) -> Result<Vec<GoodCharacter>> {
        debug!("[character] starting scan...");
        let now = SystemTime::now();

        // Create OCR pool — 3 instances for parallel talent overview reads
        let pool_size = 3;
        let ocr_backend = self.config.ocr_backend.clone();
        let ocr_pool = OcrPool::new(
            move || ocr_factory::create_ocr_model(&ocr_backend),
            pool_size,
        )?;
        debug!("[character] OCR pool: {} instances", pool_size);

        // Return to main world using BGI-style strategy:
        // press Escape one at a time, verify after each press.
        ctrl.focus_game_window();
        ctrl.return_to_main_ui(8);

        // Open character screen with retry.
        let mut screen_opened = false;
        for attempt in 0..3 {
            ctrl.key_press(enigo::Key::Layout('c'));
            utils::sleep(self.config.open_delay as u32);

            // Verify the screen opened by reading the name region.
            let ocr = ocr_pool.get();
            let check = Self::ocr_rect(&ocr, ctrl, CHAR_NAME_RECT).unwrap_or_default();
            if !check.trim().is_empty() {
                debug!("[character] character screen detected on attempt {}", attempt + 1);
                screen_opened = true;
                break;
            }

            // 'c' may have toggled it off, or we weren't in main world.
            // Return to main world again and retry.
            debug!("[character] character screen not detected (attempt {}), retrying...", attempt + 1);
            ctrl.return_to_main_ui(4);
        }
        if !screen_opened {
            error!("[character] failed to open character screen after 3 attempts");
        }

        // Jump to the specified character index
        if start_at_char > 0 {
            debug!("[character] jumping to character index {}...", start_at_char);
            for _ in 0..start_at_char {
                ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
                utils::sleep((self.config.next_delay / 2).max(100) as u32);
            }
            utils::sleep(self.config.next_delay as u32);
        }

        let mut characters: Vec<GoodCharacter> = Vec::new();
        let mut scan_metas: Vec<ScanMeta> = Vec::new();
        let mut first_name: Option<String> = None;
        let mut viewed_count = 0;
        let mut consecutive_failures = 0;
        let mut reverse = false;

        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} [{elapsed_precise}] {msg}")
                .unwrap(),
        );
        pb.set_message("0 characters scanned");

        loop {
            if utils::is_rmb_down() {
                info!("[character] user interrupted scan");
                break;
            }

            let result = self.scan_single_character(&ocr_pool, ctrl, &first_name, reverse, viewed_count);

            match result {
                Ok((Some(character), meta)) => {
                    if first_name.is_none() {
                        first_name = Some(character.key.clone());
                    }
                    let char_msg = format!(
                        "{} Lv.{} C{} {}/{}/{}{}",
                        character.key, character.level, character.constellation,
                        character.talent.auto, character.talent.skill, character.talent.burst,
                        if meta.talent_suspicious { " [talent suspicious]" } else { "" }
                    );
                    if self.config.log_progress {
                        debug!("[character] {}", char_msg);
                    }
                    characters.push(character);
                    scan_metas.push(meta);
                    consecutive_failures = 0;
                    pb.set_message(format!("{} scanned — {}", characters.len(), char_msg));
                    pb.tick();
                }
                Ok((None, _)) => {
                    // Skipped (continue_on_failure)
                    consecutive_failures += 1;
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg == "_repeat" {
                        info!("[character] loop detected, scan complete");
                        break;
                    }
                    error!("[character] scan error: {}", e);
                    consecutive_failures += 1;
                    if !self.config.continue_on_failure {
                        break;
                    }
                }
            }

            viewed_count += 1;
            if self.config.max_count > 0 && characters.len() >= self.config.max_count {
                info!("[character] reached max_count={}, stopping", self.config.max_count);
                break;
            }
            if viewed_count > 3 && characters.is_empty() {
                error!("[character] viewed {} but no results, stopping", viewed_count);
                break;
            }
            // Safety: break after too many consecutive failures (likely left character screen)
            if consecutive_failures >= 5 {
                error!("[character] {} consecutive failures, stopping scan", consecutive_failures);
                break;
            }

            // Navigate to next character
            ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
            utils::sleep(self.config.next_delay as u32);
            reverse = !reverse;
        }

        pb.finish_with_message(format!("{} characters scanned", characters.len()));

        // Close character screen
        ctrl.key_press(enigo::Key::Escape);
        utils::sleep(self.config.close_delay as u32);

        // Second pass: rescan characters with suspicious results.
        let suspicious_indices: Vec<usize> = characters.iter().enumerate()
            .filter(|(i, c)| {
                let meta = scan_metas.get(*i);
                Self::is_character_suspicious(c, meta)
            })
            .map(|(i, c)| {
                warn!(
                    "[character] suspicious result at index {}: {} Lv.{} C{} {}/{}/{}",
                    i, c.key, c.level, c.constellation,
                    c.talent.auto, c.talent.skill, c.talent.burst
                );
                i
            })
            .collect();

        if !suspicious_indices.is_empty() {
            info!(
                "[character] second pass: rescanning {} suspicious characters",
                suspicious_indices.len()
            );
            self.rescan_suspicious(ctrl, &ocr_pool, &mut characters, &suspicious_indices);
        }

        // Final sanitize: snap any remaining illegal levels to nearest valid value.
        // This runs after both passes — if OCR still produced an impossible level,
        // snap it rather than export garbage.
        for c in &mut characters {
            if (91..=94).contains(&c.level) {
                warn!("[character] {} final snap: {} → 90 (impossible level)", c.key, c.level);
                c.level = 90;
                c.ascension = level_to_ascension(90, false);
            } else if (96..=99).contains(&c.level) {
                warn!("[character] {} final snap: {} → 95 (impossible level)", c.key, c.level);
                c.level = 95;
                c.ascension = level_to_ascension(95, false);
            }
        }

        info!(
            "[character] complete, {} characters scanned in {:?}",
            characters.len(),
            now.elapsed().unwrap_or_default()
        );

        Ok(characters)
    }

    /// Get the maximum allowed base talent level for an ascension phase (0–6).
    ///
    /// Ascension phase → level cap → max talent:
    ///   0 → 20 → 1,  1 → 40 → 1,  2 → 50 → 2,  3 → 60 → 4,
    ///   4 → 70 → 6,  5 → 80 → 8,  6+ → 90+ → 10
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
    ///
    /// Uses `ScanMeta` from `scan_single_character()` which carries:
    /// - `talent_suspicious`: true if constellation subtraction hit a raw value < 4
    /// - `raw_skill` / `raw_burst`: pre-adjustment OCR values for E and Q
    fn is_character_suspicious(c: &GoodCharacter, meta: Option<&ScanMeta>) -> bool {
        // Use the same level check as read_level
        let ascended = false; // conservative — just check the level value itself
        if Self::is_level_suspicious(c.level, ascended) {
            return true;
        }

        // Raw E or Q == 1 is suspicious for characters above level 40.
        // This catches cases where OCR misread the level (e.g., Lv.10 → Lv.1).
        // We check the RAW (pre-subtraction) values, not the post-constellation values,
        // because a raw 4 minus C3/C5 bonus of 3 → 1 is perfectly valid.
        // Auto attack is excluded — many players leave it at 1.
        if c.level >= 40 {
            if let Some(m) = meta {
                if m.raw_skill == 1 || m.raw_burst == 1 {
                    return true;
                }
            }
        }

        // Talent levels too high for the character's ascension phase.
        // Uses ascension (not raw level) to correctly handle ascended characters.
        // E.g., Lv.70 ascended (phase 5, cap 80) → max talent 8, not 6.
        let max_talent = Self::max_talent_for_ascension(c.ascension);
        if c.talent.auto > max_talent || c.talent.skill > max_talent || c.talent.burst > max_talent {
            return true;
        }

        // Constellation bonus subtraction hit a raw value < 4
        if let Some(m) = meta {
            if m.talent_suspicious {
                return true;
            }
        }

        false
    }

    /// Second pass: reopen character screen, navigate to each suspicious index,
    /// and rescan level, constellation, and talents. Only updates the character
    /// if the new read is strictly better.
    #[allow(unused_assignments)]
    fn rescan_suspicious(
        &self,
        ctrl: &mut GenshinGameController,
        ocr_pool: &OcrPool,
        characters: &mut Vec<GoodCharacter>,
        suspicious_indices: &[usize],
    ) {
        // Return to main world and reopen character screen
        ctrl.return_to_main_ui(4);
        let mut screen_opened = false;
        for _attempt in 0..3 {
            ctrl.key_press(enigo::Key::Layout('c'));
            utils::sleep(self.config.open_delay as u32);
            let ocr = ocr_pool.get();
            let check = Self::ocr_rect(&ocr, ctrl, CHAR_NAME_RECT).unwrap_or_default();
            if !check.trim().is_empty() {
                screen_opened = true;
                break;
            }
            ctrl.return_to_main_ui(4);
        }
        if !screen_opened {
            warn!("[character] second pass: failed to open character screen, skipping");
            return;
        }

        // We're now at character index 0 (first character).
        // Navigate to each suspicious index by pressing right arrow.
        let mut current_index: usize = 0;

        for &target_idx in suspicious_indices {
            if utils::is_rmb_down() {
                info!("[character] second pass: user interrupted");
                break;
            }

            // Navigate forward to target
            let steps = if target_idx >= current_index {
                target_idx - current_index
            } else {
                // Wrapped around — close and reopen to reset to 0
                ctrl.key_press(enigo::Key::Escape);
                utils::sleep(self.config.close_delay as u32);
                ctrl.return_to_main_ui(4);
                ctrl.key_press(enigo::Key::Layout('c'));
                utils::sleep(self.config.open_delay as u32);
                current_index = 0;
                target_idx
            };

            for _ in 0..steps {
                ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
                utils::sleep((self.config.next_delay / 2).max(100) as u32);
            }
            if steps > 0 {
                utils::sleep(self.config.next_delay as u32);
            }
            current_index = target_idx;

            let old = &characters[target_idx];

            // Rescan: we're on the attributes tab (default after opening)
            let ocr = ocr_pool.get();

            // Verify we're looking at the right character
            let (name, _element, _raw) = self.read_name_and_element(&ocr, ctrl)
                .unwrap_or((None, None, String::new()));
            if name.as_deref() != Some(&old.key) {
                warn!(
                    "[character] rescan #{}: expected {} but got {:?}, skipping",
                    target_idx, old.key, name
                );
                continue;
            }

            // Re-read level (on attributes tab)
            let (new_level, new_ascended) = Self::read_level(&ocr, ctrl)
                .unwrap_or((old.level, false));
            let new_ascension = level_to_ascension(new_level, new_ascended);

            // Re-read constellation (navigates to constellation tab, dismisses popup)
            let new_constellation = self.read_constellation_count(&ocr, ctrl, &old.key, &None, &None)
                .unwrap_or(old.constellation);

            // Re-read talents (navigates to talents tab)
            drop(ocr);
            let raw_talents = self.read_talent_levels(ocr_pool, ctrl, &old.key, false)
                .unwrap_or((old.talent.auto, old.talent.skill, old.talent.burst));

            // Apply talent adjustments using the NEW constellation
            let (new_auto, new_skill, new_burst, _new_tsus) =
                self.adjust_talents(raw_talents.0, raw_talents.1, raw_talents.2, &old.key, new_constellation);

            // Navigate back to attributes tab for the next character
            ctrl.click_at(CHAR_TAB_ATTRIBUTES.0, CHAR_TAB_ATTRIBUTES.1);
            utils::sleep((self.config.tab_delay / 2) as u32);

            // Decide whether to use the new result
            let level_improved = new_level > old.level;
            let constellation_changed = new_constellation != old.constellation;
            let old_talent_ones = [old.talent.auto, old.talent.skill, old.talent.burst]
                .iter().filter(|&&v| v == 1).count();
            let new_talent_ones = [new_auto, new_skill, new_burst]
                .iter().filter(|&&v| v == 1).count();
            let talents_improved = new_talent_ones < old_talent_ones;

            if level_improved || constellation_changed || talents_improved {
                info!(
                    "[character] rescan #{} {}: Lv.{}->{} C{}->{} {}/{}/{}->{}/{}/{}",
                    target_idx, old.key, old.level, new_level,
                    old.constellation, new_constellation,
                    old.talent.auto, old.talent.skill, old.talent.burst,
                    new_auto, new_skill, new_burst
                );
                let c = &mut characters[target_idx];
                if level_improved {
                    c.level = new_level;
                    c.ascension = new_ascension;
                }
                if constellation_changed {
                    c.constellation = new_constellation;
                }
                // Always update talents when constellation changed (talent adjustment depends on it)
                if talents_improved || constellation_changed {
                    c.talent.auto = new_auto;
                    c.talent.skill = new_skill;
                    c.talent.burst = new_burst;
                }
            } else {
                info!(
                    "[character] rescan #{} {}: no change (Lv.{} C{} {}/{}/{})",
                    target_idx, old.key, new_level, new_constellation, new_auto, new_skill, new_burst
                );
            }
        }

        // Close character screen
        ctrl.key_press(enigo::Key::Escape);
        utils::sleep(self.config.close_delay as u32);
    }

    /// Debug scan the currently displayed character.
    ///
    /// Runs `scan_single_character` on whatever character is showing and
    /// returns a `DebugScanResult` with timing info. Used by the re-scan
    /// debug mode.
    ///
    /// The character screen must already be open and showing a character.
    pub fn debug_scan_current(
        &self,
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &mut GenshinGameController,
    ) -> DebugScanResult {
        use std::time::Instant;

        let total_start = Instant::now();
        let mut fields = Vec::new();

        // Name + element
        let t = Instant::now();
        let (name, element, raw_text) = self.read_name_and_element(ocr, ctrl)
            .unwrap_or((None, None, String::new()));
        let name_key = name.unwrap_or_default();
        fields.push(DebugOcrField {
            field_name: "name".into(),
            raw_text: raw_text,
            parsed_value: format!("{} ({})", name_key, element.as_deref().unwrap_or("?")),
            region: CHAR_NAME_RECT,
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Level
        let t = Instant::now();
        let (level, ascended) = Self::read_level(ocr, ctrl).unwrap_or((1, false));
        let ascension = level_to_ascension(level, ascended);
        fields.push(DebugOcrField {
            field_name: "level".into(),
            raw_text: String::new(),
            parsed_value: format!("lv={} ascended={} asc={}", level, ascended, ascension),
            region: CHAR_LEVEL_RECT,
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Constellation
        let t = Instant::now();
        let constellation = self.read_constellation_count(ocr, ctrl, &name_key, &element, &None)
            .unwrap_or(0);
        fields.push(DebugOcrField {
            field_name: "constellation".into(),
            raw_text: String::new(),
            parsed_value: format!("C{}", constellation),
            region: (0.0, 0.0, 0.0, 0.0),
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Talents
        let t = Instant::now();
        // Create a small pool for parallel talent overview in debug mode
        let ocr_backend = self.config.ocr_backend.clone();
        let debug_pool = OcrPool::new(
            move || ocr_factory::create_ocr_model(&ocr_backend),
            3,
        ).ok();
        let (auto, skill, burst) = if let Some(ref pool) = debug_pool {
            self.read_talent_levels(pool, ctrl, &name_key, false)
                .unwrap_or((1, 1, 1))
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
