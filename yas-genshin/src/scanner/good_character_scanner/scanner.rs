use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{bail, Result};
use image::{GenericImageView, RgbImage};
use log::{error, info, warn};
use regex::Regex;

use yas::ocr::ImageToText;
use yas::utils;

use super::GoodCharacterScannerConfig;
use crate::scanner::good_common::constants::*;
use crate::scanner::good_common::coord_scaler::CoordScaler;
use crate::scanner::good_common::fuzzy_match::fuzzy_match_map;
use crate::scanner::good_common::game_controller::GenshinGameController;
use crate::scanner::good_common::mappings::MappingManager;
use crate::scanner::good_common::models::{DebugOcrField, DebugScanResult, GoodCharacter, GoodTalent};
use crate::scanner::good_common::navigation;
use crate::scanner::good_common::ocr_factory;
use crate::scanner::good_common::ocr_pool::OcrPool;
use crate::scanner::good_common::stat_parser::level_to_ascension;

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

    /// OCR read character level, returns (level, ascended).
    fn read_level(
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &GenshinGameController,
    ) -> Result<(i32, bool)> {
        let text = Self::ocr_rect(ocr, ctrl, CHAR_LEVEL_RECT)?;

        let re = Regex::new(r"(\d+)\s*/\s*(\d+)")?;
        if let Some(caps) = re.captures(&text) {
            let level: i32 = caps[1].parse().unwrap_or(1);
            let raw_max: i32 = caps[2].parse().unwrap_or(20);
            let max_level = ((raw_max as f64 / 10.0).round() * 10.0) as i32;
            let ascended = level >= 20 && level < max_level;
            Ok((level, ascended))
        } else {
            let level = navigation::parse_number_from_text(&text);
            Ok((if level > 0 { level } else { 1 }, false))
        }
    }

    /// Click a constellation node and check if it's activated via OCR.
    fn is_constellation_activated(
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &mut GenshinGameController,
        c_index: usize,
        is_first_click: bool,
        tab_delay: u64,
    ) -> Result<bool> {
        let click_y = CHAR_CONSTELLATION_Y_BASE + c_index as f64 * CHAR_CONSTELLATION_Y_STEP;
        ctrl.click_at(CHAR_CONSTELLATION_X, click_y);

        let delay = if is_first_click { tab_delay } else { tab_delay / 2 };
        utils::sleep(delay as u32);

        let text = Self::ocr_rect(ocr, ctrl, CHAR_CONSTELLATION_ACTIVATE_RECT)?;
        // "已激活" means "Activated"
        Ok(text.contains("\u{5DF2}\u{6FC0}\u{6D3B}"))
    }

    /// Binary-search constellation count (max 3 clicks).
    fn read_constellation_count(
        &self,
        ocr: &dyn ImageToText<RgbImage>,
        ctrl: &mut GenshinGameController,
        character_name: &str,
        element: &Option<String>,
    ) -> Result<i32> {
        if NO_CONSTELLATION_CHARACTERS.contains(&character_name) {
            return Ok(0);
        }

        ctrl.click_at(CHAR_TAB_CONSTELLATION.0, CHAR_TAB_CONSTELLATION.1);
        utils::sleep(self.config.tab_delay as u32);

        let td = self.config.tab_delay;
        let constellation;

        let c3 = Self::is_constellation_activated(ocr, ctrl, 2, true, td)?;
        if !c3 {
            let c2 = Self::is_constellation_activated(ocr, ctrl, 1, false, td)?;
            if !c2 {
                let c1 = Self::is_constellation_activated(ocr, ctrl, 0, false, td)?;
                constellation = if c1 { 1 } else { 0 };
            } else {
                constellation = 2;
            }
        } else {
            let c6 = Self::is_constellation_activated(ocr, ctrl, 5, false, td)?;
            if c6 {
                constellation = 6;
            } else {
                let c4 = Self::is_constellation_activated(ocr, ctrl, 3, false, td)?;
                if !c4 {
                    constellation = 3;
                } else {
                    let c5 = Self::is_constellation_activated(ocr, ctrl, 4, false, td)?;
                    constellation = if c5 { 5 } else { 4 };
                }
            }
        }

        // Geo element: background animation may interfere with C5→C6 OCR
        let mut final_constellation = constellation;
        if constellation == 5 {
            if let Some(elem) = element {
                if elem.contains('\u{5CA9}') {
                    let c6_recheck = Self::is_constellation_activated(ocr, ctrl, 5, false, td)?;
                    if c6_recheck {
                        final_constellation = 6;
                        warn!("[character] Geo C6 recheck passed, corrected to C6");
                    }
                }
            }
        }

        ctrl.key_press(enigo::Key::Escape);
        utils::sleep(self.config.tab_delay as u32);

        Ok(final_constellation)
    }

    /// Parse "Lv.X" format from talent overview text.
    ///
    /// Port of `parseLvText()` from character_scanner.js
    fn parse_lv_text(text: &str) -> i32 {
        if text.is_empty() {
            return 0;
        }
        let re = Regex::new(r"(?i)[Ll][Vv]\.?\s*(\d{1,2})").unwrap();
        if let Some(caps) = re.captures(text) {
            let lv: i32 = caps[1].parse().unwrap_or(0);
            if (1..=15).contains(&lv) {
                return lv;
            }
        }
        0
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

        let delay = if is_first { tab_delay } else { tab_delay / 2 };
        utils::sleep(delay as u32);

        let text = Self::ocr_rect(ocr, ctrl, CHAR_TALENT_LEVEL_RECT)?;
        let re = Regex::new(r"(\d+)")?;
        if let Some(caps) = re.captures(&text) {
            let v: i32 = caps[1].parse().unwrap_or(1);
            if (1..=15).contains(&v) {
                return Ok(v);
            }
        }
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
                    .map(|t| Self::parse_lv_text(&t))
                    .unwrap_or(0)
            },
            || {
                rayon::join(
                    || {
                        let ocr = ocr_pool.get();
                        Self::ocr_image_region(&ocr, &image, CHAR_TALENT_OVERVIEW_SKILL, &scaler)
                            .map(|t| Self::parse_lv_text(&t))
                            .unwrap_or(0)
                    },
                    || {
                        let ocr = ocr_pool.get();
                        Self::ocr_image_region(&ocr, &image, burst_rect, &scaler)
                            .map(|t| Self::parse_lv_text(&t))
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
            utils::sleep(500);
        }

        Ok((auto, skill, burst))
    }

    /// Scan a single character.
    ///
    /// `first_name`: the first character's key for loop detection (None on first scan).
    /// `reverse`: if true, scan in talents→constellation→attributes order.
    ///
    /// Returns `Ok(Some(character))` on success, `Ok(None)` to skip,
    /// or a special marker via `first_name` match detection.
    ///
    /// Port of `scanSingleCharacter()` from character_scanner.js
    fn scan_single_character(
        &self,
        ocr_pool: &OcrPool,
        ctrl: &mut GenshinGameController,
        first_name: &Option<String>,
        reverse: bool,
    ) -> Result<Option<GoodCharacter>> {
        let ocr = ocr_pool.get();

        // Name and element are visible from any tab
        let (name, element, raw_text) = self.read_name_and_element(&ocr, ctrl)?;

        let name = match name {
            Some(n) => n,
            None => {
                if self.config.continue_on_failure {
                    warn!("[character] cannot identify: \u{300C}{}\u{300D}, skipping", raw_text);
                    return Ok(None);
                }
                bail!("Cannot identify character: \u{300C}{}\u{300D}", raw_text);
            }
        };

        // Loop detection
        if let Some(first) = first_name {
            if &name == first {
                return Err(anyhow::anyhow!("_repeat"));
            }
        }

        let level_info;
        let constellation;
        let talents;

        if !reverse {
            // Forward: attributes → constellation → talents (already on attributes tab)
            level_info = Self::read_level(&ocr, ctrl)?;
            constellation = self.read_constellation_count(&ocr, ctrl, &name, &element)?;
            // Drop the single OCR guard before talent reading (which uses pool internally)
            drop(ocr);
            talents = self.read_talent_levels(ocr_pool, ctrl, &name, false)?;
        } else {
            // Reverse: talents → constellation → attributes (already on talents tab)
            // Drop the single OCR guard before talent reading (which uses pool internally)
            drop(ocr);
            talents = self.read_talent_levels(ocr_pool, ctrl, &name, true)?;
            let ocr = ocr_pool.get();
            constellation = self.read_constellation_count(&ocr, ctrl, &name, &element)?;
            ctrl.click_at(CHAR_TAB_ATTRIBUTES.0, CHAR_TAB_ATTRIBUTES.1);
            utils::sleep(self.config.tab_delay as u32);
            level_info = Self::read_level(&ocr, ctrl)?;
        }

        let (level, ascended) = level_info;
        let ascension = level_to_ascension(level, ascended);

        let mut auto = talents.0;
        let mut skill = talents.1;
        let mut burst = talents.2;

        // Tartaglia innate talent: auto +1 bonus
        if name == TARTAGLIA_KEY {
            auto = (auto - 1).max(1);
        }

        // Subtract constellation talent bonuses (C3/C5 each add +3)
        if let Some(bonus) = self.mappings.character_const_bonus.get(&name) {
            if constellation >= 3 {
                if let Some(ref c3_type) = bonus.c3 {
                    match c3_type.as_str() {
                        "A" => auto = (auto - 3).max(1),
                        "E" => skill = (skill - 3).max(1),
                        "Q" => burst = (burst - 3).max(1),
                        _ => {}
                    }
                }
            }
            if constellation >= 5 {
                if let Some(ref c5_type) = bonus.c5 {
                    match c5_type.as_str() {
                        "A" => auto = (auto - 3).max(1),
                        "E" => skill = (skill - 3).max(1),
                        "Q" => burst = (burst - 3).max(1),
                        _ => {}
                    }
                }
            }
        }

        Ok(Some(GoodCharacter {
            key: name,
            level,
            constellation,
            ascension,
            talent: GoodTalent {
                auto,
                skill,
                burst,
            },
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
        info!("[character] starting scan...");
        let now = SystemTime::now();

        // Create OCR pool — 3 instances for parallel talent overview reads
        let pool_size = 3;
        let ocr_backend = self.config.ocr_backend.clone();
        let ocr_pool = OcrPool::new(
            move || ocr_factory::create_ocr_model(&ocr_backend),
            pool_size,
        )?;
        info!("[character] OCR pool: {} instances", pool_size);

        // Open character screen
        ctrl.key_press(enigo::Key::Layout('c'));
        utils::sleep((self.config.open_delay as f64 * 1.5) as u32);

        // Jump to the specified character index
        if start_at_char > 0 {
            info!("[character] jumping to character index {}...", start_at_char);
            for _ in 0..start_at_char {
                ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
                utils::sleep((self.config.tab_delay / 2).max(100) as u32);
            }
            utils::sleep(self.config.tab_delay as u32);
        }

        let mut characters: Vec<GoodCharacter> = Vec::new();
        let mut first_name: Option<String> = None;
        let mut viewed_count = 0;
        let mut reverse = false;

        loop {
            if utils::is_rmb_down() {
                info!("[character] user interrupted scan");
                break;
            }

            let result = self.scan_single_character(&ocr_pool, ctrl, &first_name, reverse);

            match result {
                Ok(Some(character)) => {
                    if first_name.is_none() {
                        first_name = Some(character.key.clone());
                    }
                    if self.config.log_progress {
                        info!(
                            "[character] {} Lv.{} C{} {}/{}/{}",
                            character.key, character.level, character.constellation,
                            character.talent.auto, character.talent.skill, character.talent.burst
                        );
                    }
                    characters.push(character);
                }
                Ok(None) => {
                    // Skipped (continue_on_failure)
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg == "_repeat" {
                        info!("[character] loop detected, scan complete");
                        break;
                    }
                    error!("[character] scan error: {}", e);
                    if !self.config.continue_on_failure {
                        break;
                    }
                }
            }

            viewed_count += 1;
            if viewed_count > 3 && characters.is_empty() {
                error!("[character] viewed {} but no results, stopping", viewed_count);
                break;
            }

            // Navigate to next character
            ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
            utils::sleep(self.config.tab_delay as u32);
            reverse = !reverse;
        }

        // Close character screen
        ctrl.key_press(enigo::Key::Escape);
        utils::sleep(500);

        info!(
            "[character] complete, {} characters scanned in {:?}",
            characters.len(),
            now.elapsed().unwrap_or_default()
        );

        Ok(characters)
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
        let constellation = self.read_constellation_count(ocr, ctrl, &name_key, &element)
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

        let character = GoodCharacter {
            key: name_key,
            level,
            constellation,
            ascension,
            talent: GoodTalent { auto, skill, burst },
        };
        let parsed_json = serde_json::to_string_pretty(&character).unwrap_or_default();

        DebugScanResult {
            fields,
            total_duration_ms: total_start.elapsed().as_millis() as u64,
            parsed_json,
        }
    }
}
