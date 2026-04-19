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
use crate::scanner::common::debug_dump::DumpCollector;
use crate::scanner::common::fuzzy_match::fuzzy_match_map;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::MappingManager;
use crate::scanner::common::models::{DebugOcrField, DebugScanResult, GoodCharacter, GoodTalent};
use crate::scanner::common::ocr_factory;
use crate::scanner::common::ocr_pool::{OcrPool, SharedOcrPools};
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
/// Tip logged after OCR failures that are likely caused by slow UI transitions.
const DELAY_TIP: &str = "[tip] 如果扫描器切换过快，请在设置中增大「面板切换」或「切换角色」延迟 / \
    [tip] If the scanner moves too fast, try increasing the \"Panel switch\" or \"Next character\" delay in settings";

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
        name_v5_ocr: Option<&dyn ImageToText<RgbImage>>,
        ctrl: &GenshinGameController,
    ) -> Result<(Option<String>, Option<String>, String)> {
        // Try v5 first for character name — v4 dict lacks some characters (e.g. 魈/Xiao).
        // v5 is strictly better on names per eval (111 correct vs 110 for v4).
        if let Some(v5) = name_v5_ocr {
            let text_v5 = Self::ocr_rect(v5, ctrl, CHAR_NAME_RECT)?;
            let (name_v5, element_v5) = self.parse_name_and_element(&text_v5);
            if name_v5.is_some() {
                log_debug!("[character] 名字OCR (v5): {:?} -> {:?}", "[character] name OCR (v5): {:?} -> {:?}", text_v5, name_v5);
                return Ok((name_v5, element_v5, text_v5));
            }
        }

        // v4 fallback (or primary if no v5 available)
        let text = Self::ocr_rect(ocr, ctrl, CHAR_NAME_RECT)?;
        let (name, element) = self.parse_name_and_element(&text);
        if name.is_some() {
            log_debug!("[character] 名字OCR (v4): {:?} -> {:?}", "[character] name OCR (v4): {:?} -> {:?}", text, name);
            return Ok((name, element, text));
        }

        log_debug!("[character] 首次名字匹配失败: \u{300C}{}\u{300D}，重试中...", "[character] first name match failed: \u{300C}{}\u{300D}, retrying...", text);
        utils::sleep(1000);

        // Retry: v5 first, then v4
        if let Some(v5) = name_v5_ocr {
            let text_v5 = Self::ocr_rect(v5, ctrl, CHAR_NAME_RECT)?;
            let (name_v5, element_v5) = self.parse_name_and_element(&text_v5);
            if name_v5.is_some() {
                log_debug!("[character] 名字重试 (v5): {:?} -> {:?}", "[character] name retry (v5): {:?} -> {:?}", text_v5, name_v5);
                return Ok((name_v5, element_v5, text_v5));
            }
        }

        let text2 = Self::ocr_rect(ocr, ctrl, CHAR_NAME_RECT)?;
        let (name2, element2) = self.parse_name_and_element(&text2);
        if name2.is_none() {
            log_debug!("[character] 第二次名字匹配失败: \u{300C}{}\u{300D}", "[character] second name match failed: \u{300C}{}\u{300D}", text2);
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
                log_debug!("[character] 等级OCR回退拆分: {:?} -> {}/{}", "[character] level OCR fallback split: {:?} -> {}/{}", digits, lv, mx);
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
                    log_debug!(
                        "[character] 等级OCR去噪拆分: {:?} (移除索引 {}) -> {}/{}",
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
                            log_debug!("[character] 等级OCR部分提取: {:?} -> {}", "[character] level OCR partial extract: {:?} -> {}", digits, lv);
                            return Ok((lv, false));
                        }
                    }
                }
            }
        }

        log_warn!("[character] 等级OCR完全失败: {:?}", "[character] level OCR completely failed: {:?}", text);
        log_info!("[tip] 如果扫描器切换过快，请在设置中增大「面板切换」或「切换角色」延迟", "[tip] If the scanner moves too fast, try increasing the \"Panel switch\" or \"Next character\" delay in settings");
        // Save the level region for debugging
        if let Ok(im) = ctrl.capture_region(CHAR_LEVEL_RECT.0, CHAR_LEVEL_RECT.1, CHAR_LEVEL_RECT.2, CHAR_LEVEL_RECT.3) {
            let ts = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
            let path = format!("debug_level_fail_{}.png", ts);
            let _ = im.save(&path);
            log_info!("[character] 已保存失败的等级区域到 {}", "[character] saved failed level region to {}", path);
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
            log_debug!(
                "[character] 等级 {} (最大={}, 突破={}) 可能需要重新读取",
                "[character] level {} (max={}, ascended={}) may need re-reading",
                level, max_level, ascended
            );
        }

        Ok((level, ascended))
    }

    /// Click a constellation node and check if it's activated via OCR.
    /// Used as fallback when pixel detection returns a non-monotonic result.
    fn is_constellation_activated(
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &mut GenshinGameController,
        c_index: usize,
        is_first_click: bool,
        tab_delay: u64,
        dump: &mut Option<DumpCollector>,
    ) -> Result<bool> {
        let click_y = CHAR_CONSTELLATION_Y_BASE + c_index as f64 * CHAR_CONSTELLATION_Y_STEP;
        ctrl.click_at(CHAR_CONSTELLATION_X, click_y);

        let delay = if is_first_click { tab_delay * 3 / 4 } else { tab_delay / 2 };
        utils::sleep(delay as u32);

        let text = Self::ocr_rect(ocr, ctrl, CHAR_CONSTELLATION_ACTIVATE_RECT)?;
        let activated = text.contains("\u{5DF2}\u{6FC0}\u{6D3B}");

        if let Some(ref mut d) = dump {
            let field = format!("constellation_c{}", c_index + 1);
            d.record_ocr(0, &field, CHAR_CONSTELLATION_ACTIVATE_RECT, &text);
            d.set_final_result(&field, if activated { "activated" } else { "locked" });
        }

        Ok(activated)
    }

    /// OCR binary-search constellation count (max 3 clicks). Used as fallback.
    fn read_constellation_ocr(
        &self,
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &mut GenshinGameController,
        dump: &mut Option<DumpCollector>,
    ) -> Result<i32> {
        let td = self.config.tab_delay;

        let c3 = Self::is_constellation_activated(ocr, ctrl, 2, true, td, dump)?;
        let constellation = if !c3 {
            let c2 = Self::is_constellation_activated(ocr, ctrl, 1, false, td, dump)?;
            if !c2 {
                let c1 = Self::is_constellation_activated(ocr, ctrl, 0, false, td, dump)?;
                if c1 { 1 } else { 0 }
            } else {
                2
            }
        } else {
            let c6 = Self::is_constellation_activated(ocr, ctrl, 5, false, td, dump)?;
            if c6 {
                6
            } else {
                let c4 = Self::is_constellation_activated(ocr, ctrl, 3, false, td, dump)?;
                if !c4 {
                    3
                } else {
                    let c5 = Self::is_constellation_activated(ocr, ctrl, 4, false, td, dump)?;
                    if c5 { 5 } else { 4 }
                }
            }
        };

        // Dismiss the constellation detail popup
        ctrl.key_press(enigo::Key::Escape);
        utils::sleep(self.config.tab_delay as u32);

        Ok(constellation)
    }

    /// Pixel-based constellation detection with OCR fallback on non-monotonic results.
    ///
    /// Normal path: capture constellation tab screenshot, check ring brightness at all 6
    /// icon positions (0 clicks). Falls back to OCR binary search (max 3 clicks) if the
    /// pixel result is non-monotonic (A-L-A pattern), indicating a detection error.
    fn read_constellation_count(
        &self,
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &mut GenshinGameController,
        character_name: &str,
        _element: &Option<String>,
        dump: &mut Option<DumpCollector>,
    ) -> Result<i32> {
        if NO_CONSTELLATION_CHARACTERS.contains(&character_name) {
            return Ok(0);
        }

        ctrl.click_at(CHAR_TAB_CONSTELLATION.0, CHAR_TAB_CONSTELLATION.1);
        utils::sleep(self.config.tab_delay as u32);

        let image = ctrl.capture_game()?;

        if let Some(ref mut d) = dump {
            d.add_image("constellation", &image);
        }

        let (constellation, monotonic) = crate::scanner::common::pixel_utils::detect_constellation_pixel(
            &image, &ctrl.scaler,
        );

        if monotonic {
            if let Some(ref mut d) = dump {
                // Record the pixel-based constellation result
                // Use image index from the constellation image we just added
                let img_idx = d.images_len() - 1;
                d.record_pixel(img_idx, "constellation_pixel", (960.0, 540.0), [0, 0, 0],
                    &format!("C{} (monotonic)", constellation));
            }
            Ok(constellation)
        } else {
            log_debug!(
                "[constellation] 像素非单调 {}，回退到OCR二分搜索",
                "[constellation] pixel non-monotonic for {}, falling back to OCR binary search",
                character_name
            );
            self.read_constellation_ocr(ocr, ctrl, dump)
        }
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
        log_debug!("[talent] 点击回退 idx={} 原始OCR: {:?}", "[talent] click fallback idx={} raw OCR: {:?}", talent_index, text);
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
        log_warn!("[talent] 点击回退失败 idx={}，默认为1", "[talent] click fallback failed for idx={}, defaulting to 1", talent_index);
        log_info!("[tip] 如果扫描器切换过快，请在设置中增大「面板切换」或「切换角色」延迟", "[tip] If the scanner moves too fast, try increasing the \"Panel switch\" or \"Next character\" delay in settings");
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
                    .map(|t| { let lv = Self::parse_lv_text(&t); log_debug!("[talent] 概览普攻: 「{}」 → {}", "[talent] overview auto: 「{}」 → {}", t.trim(), lv); lv })
                    .unwrap_or(0)
            },
            || {
                rayon::join(
                    || {
                        let ocr = ocr_pool.get();
                        Self::ocr_image_region(&ocr, &image, CHAR_TALENT_OVERVIEW_SKILL, &scaler)
                            .map(|t| { let lv = Self::parse_lv_text(&t); log_debug!("[talent] 概览战技: 「{}」 → {}", "[talent] overview skill: 「{}」 → {}", t.trim(), lv); lv })
                            .unwrap_or(0)
                    },
                    || {
                        let ocr = ocr_pool.get();
                        Self::ocr_image_region(&ocr, &image, burst_rect, &scaler)
                            .map(|t| { let lv = Self::parse_lv_text(&t); log_debug!("[talent] 概览爆发: 「{}」 → {}", "[talent] overview burst: 「{}」 → {}", t.trim(), lv); lv })
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
            log_debug!(
                "[character] 天赋概览失败: {}，使用点击回退",
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
        name_fallback_ocr: Option<&dyn ImageToText<RgbImage>>,
        ctrl: &mut GenshinGameController,
        first_name: &Option<String>,
        reverse: bool,
        char_index: usize,
    ) -> Result<(Option<GoodCharacter>, ScanMeta)> {
        let ocr = ocr_pool.get();

        // Name and element are visible from any tab
        let (name, element, raw_text) = self.read_name_and_element(&ocr, name_fallback_ocr, ctrl)?;

        let name = match name {
            Some(n) => n,
            None => {
                if self.config.continue_on_failure {
                    log_warn!("[character] 无法识别: \u{300C}{}\u{300D}，跳过", "[character] cannot identify: \u{300C}{}\u{300D}, skipping", raw_text);
                    log_info!("[tip] 如果扫描器切换过快，请在设置中增大「面板切换」或「切换角色」延迟", "[tip] If the scanner moves too fast, try increasing the \"Panel switch\" or \"Next character\" delay in settings");
                    return Ok((None, ScanMeta { talent_suspicious: false, raw_skill: 0, raw_burst: 0 }));
                }
                bail!("无法识别角色 / Cannot identify character: \u{300C}{}\u{300D}\n{}", raw_text, DELAY_TIP);
            }
        };

        // Loop detection
        if let Some(first) = first_name {
            if &name == first {
                return Err(anyhow::anyhow!("_repeat"));
            }
        }

        // Set up dump collector if image dumping is enabled
        let mut dump = if self.config.dump_images {
            Some(DumpCollector::new("debug_images", "characters", char_index, &ctrl.scaler))
        } else {
            None
        };

        let level_info;
        let constellation;
        let talents;

        if !reverse {
            // Forward: attributes → constellation → talents (already on attributes tab)

            // Capture and record the attributes screen (name + level visible)
            if let Some(ref mut d) = dump {
                if let Ok(img) = ctrl.capture_game() {
                    let idx = d.add_image("attributes", &img);
                    d.record_ocr(idx, "name", CHAR_NAME_RECT, &raw_text);
                    d.set_final_result("name", &name);
                }
            }

            level_info = Self::read_level(&ocr, ctrl)?;
            if let Some(ref mut d) = dump {
                d.record_ocr(0, "level", CHAR_LEVEL_RECT, "");
                d.set_final_result("level", &format!("lv={} asc={}", level_info.0, level_info.1));
            }

            constellation = self.read_constellation_count(&ocr, ctrl, &name, &element, &mut dump)?;

            // Drop the single OCR guard before talent reading (which uses pool internally)
            drop(ocr);
            talents = self.read_talent_levels(ocr_pool, ctrl, &name, false)?;

            // Capture and record the talent overview screen
            if let Some(ref mut d) = dump {
                if let Ok(img) = ctrl.capture_game() {
                    let idx = d.add_image("talents", &img);
                    let has_special = SPECIAL_BURST_CHARACTERS.contains(&name.as_str());
                    let burst_rect = if has_special { CHAR_TALENT_OVERVIEW_BURST_SPECIAL } else { CHAR_TALENT_OVERVIEW_BURST };
                    d.record_ocr(idx, "talent_auto", CHAR_TALENT_OVERVIEW_AUTO, "");
                    d.set_final_result("talent_auto", &format!("{}", talents.0));
                    d.record_ocr(idx, "talent_skill", CHAR_TALENT_OVERVIEW_SKILL, "");
                    d.set_final_result("talent_skill", &format!("{}", talents.1));
                    d.record_ocr(idx, "talent_burst", burst_rect, "");
                    d.set_final_result("talent_burst", &format!("{}", talents.2));
                }
            }
        } else {
            // Reverse: talents → constellation → attributes (already on talents tab)
            // Drop the single OCR guard before talent reading (which uses pool internally)
            drop(ocr);
            talents = self.read_talent_levels(ocr_pool, ctrl, &name, true)?;

            // Capture and record the talent overview screen
            if let Some(ref mut d) = dump {
                if let Ok(img) = ctrl.capture_game() {
                    let idx = d.add_image("talents", &img);
                    let has_special = SPECIAL_BURST_CHARACTERS.contains(&name.as_str());
                    let burst_rect = if has_special { CHAR_TALENT_OVERVIEW_BURST_SPECIAL } else { CHAR_TALENT_OVERVIEW_BURST };
                    d.record_ocr(idx, "talent_auto", CHAR_TALENT_OVERVIEW_AUTO, "");
                    d.set_final_result("talent_auto", &format!("{}", talents.0));
                    d.record_ocr(idx, "talent_skill", CHAR_TALENT_OVERVIEW_SKILL, "");
                    d.set_final_result("talent_skill", &format!("{}", talents.1));
                    d.record_ocr(idx, "talent_burst", burst_rect, "");
                    d.set_final_result("talent_burst", &format!("{}", talents.2));
                }
            }

            let ocr = ocr_pool.get();
            constellation = self.read_constellation_count(&ocr, ctrl, &name, &element, &mut dump)?;

            ctrl.click_at(CHAR_TAB_ATTRIBUTES.0, CHAR_TAB_ATTRIBUTES.1);
            utils::sleep(self.config.tab_delay as u32);
            level_info = Self::read_level(&ocr, ctrl)?;

            // Capture and record the attributes screen
            if let Some(ref mut d) = dump {
                if let Ok(img) = ctrl.capture_game() {
                    let idx = d.add_image("attributes", &img);
                    d.record_ocr(idx, "name", CHAR_NAME_RECT, &raw_text);
                    d.set_final_result("name", &name);
                    d.record_ocr(idx, "level", CHAR_LEVEL_RECT, "");
                    d.set_final_result("level", &format!("lv={} asc={}", level_info.0, level_info.1));
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

        let character = GoodCharacter {
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
        };

        // Record constellation and adjusted talents in dump
        if let Some(ref mut d) = dump {
            d.record_ocr(0, "constellation_final", (0.0, 0.0, 0.0, 0.0), "");
            d.set_final_result("constellation_final", &format!("C{}", constellation));
            d.record_ocr(0, "talents_adjusted", (0.0, 0.0, 0.0, 0.0), "");
            d.set_final_result("talents_adjusted",
                &format!("auto={} skill={} burst={}", auto, skill, burst));
        }

        if let Some(d) = dump {
            let json = serde_json::to_string_pretty(&character).unwrap_or_default();
            d.finalize_success(&json);
        }

        Ok((Some(character), ScanMeta {
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
    pub fn scan(&self, ctrl: &mut GenshinGameController, start_at_char: usize, pools: &SharedOcrPools) -> Result<Vec<GoodCharacter>> {
        log_debug!("[character] 开始扫描...", "[character] starting scan...");
        let now = SystemTime::now();

        // Use shared OCR pools (v4 for primary, v5 for name fallback).
        let ocr_pool = pools.v4().clone();
        // Hold a v5 instance for the entire scan (character scanner is sequential).
        let name_fallback_guard = pools.v5().get();

        // Return to main world using BGI-style strategy:
        // press Escape one at a time, verify after each press.
        ctrl.focus_game_window();
        if ctrl.check_rmb() { anyhow::bail!("cancelled"); }
        ctrl.return_to_main_ui(8);
        if ctrl.check_rmb() { anyhow::bail!("cancelled"); }

        // Open character screen with retry.
        let mut screen_opened = false;
        for attempt in 0..3 {
            if ctrl.check_rmb() { anyhow::bail!("cancelled"); }
            ctrl.key_press(enigo::Key::Layout('c'));
            utils::sleep(self.config.open_delay as u32);

            // Verify the screen opened by reading the name region.
            let ocr = ocr_pool.get();
            let check = Self::ocr_rect(&ocr, ctrl, CHAR_NAME_RECT).unwrap_or_default();
            if !check.trim().is_empty() {
                log_debug!("[character] 角色界面已检测到，第{}次尝试", "[character] character screen detected on attempt {}", attempt + 1);
                screen_opened = true;
                break;
            }

            // 'c' may have toggled it off, or we weren't in main world.
            // Return to main world again and retry.
            log_debug!("[character] 未检测到角色界面（第{}次尝试），重试中...", "[character] character screen not detected (attempt {}), retrying...", attempt + 1);
            ctrl.return_to_main_ui(4);
        }
        if !screen_opened {
            log_error!("[character] 3次尝试后仍无法打开角色界面", "[character] failed to open character screen after 3 attempts");
            log_info!("[tip] 如果扫描器切换过快，请在设置中增大「面板切换」或「切换角色」延迟", "[tip] If the scanner moves too fast, try increasing the \"Panel switch\" or \"Next character\" delay in settings");
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
            if ctrl.check_rmb() {
                log_info!("[character] 用户中断扫描", "[character] user interrupted scan");
                break;
            }

            let result = self.scan_single_character(&ocr_pool, Some(&name_fallback_guard as &dyn ImageToText<RgbImage>), ctrl, &first_name, reverse, viewed_count);

            match result {
                Ok((Some(character), meta)) => {
                    if first_name.is_none() {
                        first_name = Some(character.key.clone());
                    }
                    let char_msg = format!(
                        "{} Lv.{} C{} {}/{}/{}{}",
                        character.key, character.level, character.constellation,
                        character.talent.auto, character.talent.skill, character.talent.burst,
                        if meta.talent_suspicious { " [will verify]" } else { "" }
                    );
                    if self.config.log_progress {
                        log_debug!("[character] {}", "[character] {}", char_msg);
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
                        log_info!("[character] 检测到循环，扫描完成", "[character] loop detected, scan complete");
                        break;
                    }
                    log_error!("[character] 扫描错误: {}", "[character] scan error: {}", e);
                    consecutive_failures += 1;
                    if !self.config.continue_on_failure {
                        break;
                    }
                }
            }

            viewed_count += 1;
            if self.config.max_count > 0 && characters.len() >= self.config.max_count {
                log_info!("[character] 已达到最大数量={}，停止", "[character] reached max_count={}, stopping", self.config.max_count);
                break;
            }
            if viewed_count > 3 && characters.is_empty() {
                log_error!("[character] 已查看{}个但无结果，停止", "[character] viewed {} but no results, stopping", viewed_count);
                log_info!("[tip] 如果扫描器切换过快，请在设置中增大「面板切换」或「切换角色」延迟", "[tip] If the scanner moves too fast, try increasing the \"Panel switch\" or \"Next character\" delay in settings");
                break;
            }
            // Safety: break after too many consecutive failures (likely left character screen)
            if consecutive_failures >= 5 {
                log_error!("[character] 连续{}次失败，停止扫描", "[character] {} consecutive failures, stopping scan", consecutive_failures);
                log_info!("[tip] 如果扫描器切换过快，请在设置中增大「面板切换」或「切换角色」延迟", "[tip] If the scanner moves too fast, try increasing the \"Panel switch\" or \"Next character\" delay in settings");
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
                log_debug!(
                    "[character] 将重新读取索引{}: {} Lv.{} A{} C{} {}/{}/{}",
                    "[character] will re-read index {}: {} Lv.{} A{} C{} {}/{}/{}",
                    i, c.key, c.level, c.ascension, c.constellation,
                    c.talent.auto, c.talent.skill, c.talent.burst
                );
                i
            })
            .collect();

        if !suspicious_indices.is_empty() && !ctrl.is_cancelled() {
            log_info!(
                "[character] 第二轮: 重新读取{}个角色以提高精度",
                "[character] second pass: re-reading {} characters for accuracy",
                suspicious_indices.len()
            );
            self.rescan_suspicious(ctrl, &ocr_pool, Some(&name_fallback_guard as &dyn ImageToText<RgbImage>), &mut characters, &suspicious_indices);
        }

        // Final sanitize: snap any remaining illegal levels to nearest valid value.
        // This runs after both passes — if OCR still produced an impossible level,
        // snap it rather than export garbage.
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
            log_info!("[tip] 如果扫描器切换过快，请在设置中增大「面板切换」或「切换角色」延迟", "[tip] If the scanner moves too fast, try increasing the \"Panel switch\" or \"Next character\" delay in settings");
        }

        if ctrl.is_cancelled() {
            log_info!(
                "[character] 已中断，扫描了{}个角色，耗时{:?}",
                "[character] interrupted, {} characters scanned in {:?}",
                characters.len(),
                now.elapsed().unwrap_or_default()
            );
        } else {
            log_info!(
                "[character] 完成，扫描了{}个角色，耗时{:?}",
                "[character] complete, {} characters scanned in {:?}",
                characters.len(),
                now.elapsed().unwrap_or_default()
            );
        }

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
        name_fallback_ocr: Option<&dyn ImageToText<RgbImage>>,
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
            log_warn!("[character] 第二轮: 无法打开角色界面，跳过", "[character] second pass: failed to open character screen, skipping");
            return;
        }

        // We're now at character index 0 (first character).
        // Navigate to each suspicious index by pressing right arrow.
        let mut current_index: usize = 0;

        for &target_idx in suspicious_indices {
            if ctrl.check_rmb() {
                log_info!("[character] 第二轮: 用户中断", "[character] second pass: user interrupted");
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
            let (name, _element, _raw) = self.read_name_and_element(&ocr, name_fallback_ocr, ctrl)
                .unwrap_or((None, None, String::new()));
            if name.as_deref() != Some(&old.key) {
                log_info!(
                    "[character] 验证 #{}: 期望 {} 但读到 {:?}，跳过",
                    "[character] verify #{}: expected {} but read {:?}, skipping",
                    target_idx, old.key, name
                );
                continue;
            }

            // Re-read level (on attributes tab)
            let (new_level, new_ascended) = Self::read_level(&ocr, ctrl)
                .unwrap_or((old.level, false));
            let new_ascension = level_to_ascension(new_level, new_ascended);

            // Re-read constellation (navigates to constellation tab, dismisses popup)
            let new_constellation = self.read_constellation_count(&ocr, ctrl, &old.key, &None, &mut None)
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
                        new_auto, new_skill, new_burst));
                }
                log_info!(
                    "[character] 验证 #{} {}: {}",
                    "[character] verify #{} {}: {}",
                    target_idx, old.key, changes.join(", ")
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
                log_debug!(
                    "[character] 验证 #{} {}: 无变化",
                    "[character] verify #{} {}: no change",
                    target_idx, old.key
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
        let (name, element, raw_text) = self.read_name_and_element(ocr, None, ctrl)
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
        let constellation = self.read_constellation_count(ocr, ctrl, &name_key, &element, &mut None)
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
