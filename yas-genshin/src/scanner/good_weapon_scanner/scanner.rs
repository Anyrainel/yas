use std::rc::Rc;
use std::time::SystemTime;

use anyhow::{bail, Result};
use image::{GenericImageView, RgbImage};
use log::{error, info, warn};
use regex::Regex;

use yas::ocr::ImageToText;

use super::GoodWeaponScannerConfig;
use crate::scanner::good_common::backpack_scanner::{BackpackScanConfig, BackpackScanner, GridEvent, ScanAction};
use crate::scanner::good_common::constants::*;
use crate::scanner::good_common::coord_scaler::CoordScaler;
use crate::scanner::good_common::fuzzy_match::fuzzy_match_map;
use crate::scanner::good_common::game_controller::GenshinGameController;
use crate::scanner::good_common::mappings::MappingManager;
use crate::scanner::good_common::models::{DebugOcrField, DebugScanResult, GoodWeapon};
use crate::scanner::good_common::navigation;
use crate::scanner::good_common::ocr_factory;
use crate::scanner::good_common::pixel_utils;
use crate::scanner::good_common::stat_parser::level_to_ascension;

/// Computed OCR regions for weapon card (at 1920x1080 base).
/// Port of the WEAPON_OCR calculation from GOODScanner/lib/weapon_scanner.js
struct WeaponOcrRegions {
    name: (f64, f64, f64, f64),
    level: (f64, f64, f64, f64),
    refinement: (f64, f64, f64, f64),
    equip: (f64, f64, f64, f64),
}

impl WeaponOcrRegions {
    fn new() -> Self {
        let card_x: f64 = 1307.0;
        let card_y: f64 = 119.0;
        let card_w: f64 = 494.0;
        let card_h: f64 = 841.0;

        Self {
            name: (card_x, card_y, card_w, (card_h * 0.07).round()),
            level: (
                card_x + (card_w * 0.060).round(),
                card_y + (card_h * 0.367).round(),
                (card_w * 0.262).round(),
                (card_h * 0.035).round(),
            ),
            refinement: (
                card_x + (card_w * 0.058).round(),
                card_y + (card_h * 0.417).round(),
                (card_w * 0.25).round(),
                (card_h * 0.038).round(),
            ),
            equip: (
                card_x + (card_w * 0.10).round(),
                card_y + (card_h * 0.935).round(),
                (card_w * 0.85).round(),
                (card_h * 0.06).round(),
            ),
        }
    }
}

/// Weapon scanner ported from GOODScanner/lib/weapon_scanner.js.
///
/// Scans weapons from the backpack grid, detecting name/level/refinement/equip
/// via OCR on captured game images. Stops when low-tier weapons are reached.
///
/// The scanner holds only business logic (OCR model, mappings, config).
/// The game controller is passed to `scan()` to avoid borrow checker conflicts
/// with `BackpackScanner`.
pub struct GoodWeaponScanner {
    config: GoodWeaponScannerConfig,
    ocr_model: Box<dyn ImageToText<RgbImage> + Send>,
    mappings: Rc<MappingManager>,
    ocr_regions: WeaponOcrRegions,
}

/// Additional forging material stop names not in the shared constants
const WEAPON_FORGING_STOP_NAMES: &[&str] = &[
    "\u{7CBE}\u{953B}\u{7528}\u{9B54}\u{77FF}", // 精锻用魔矿
    "\u{7CBE}\u{953B}\u{7528}\u{826F}\u{77FF}", // 精锻用良矿
    "\u{7CBE}\u{953B}\u{7528}\u{6742}\u{77FF}", // 精锻用杂矿
];

/// Result of scanning a single weapon: weapon data, stop signal, or skip
enum WeaponScanResult {
    Weapon(GoodWeapon),
    Stop,
    Skip,
}

impl GoodWeaponScanner {
    pub fn new(
        config: GoodWeaponScannerConfig,
        mappings: Rc<MappingManager>,
    ) -> Result<Self> {
        let ocr_model = ocr_factory::create_ocr_model(&config.ocr_backend)?;

        Ok(Self {
            config,
            ocr_model,
            mappings,
            ocr_regions: WeaponOcrRegions::new(),
        })
    }
}

impl GoodWeaponScanner {
    /// OCR a sub-region of a captured game image.
    /// Crops the sub-region, converts to RgbImage, and runs OCR.
    fn ocr_image_region(
        &self,
        image: &RgbImage,
        rect: (f64, f64, f64, f64),
        scaler: &CoordScaler,
    ) -> Result<String> {
        let (bx, by, bw, bh) = rect;
        let x = scaler.x(bx) as u32;
        let y = scaler.y(by) as u32;
        let w = scaler.x(bw) as u32;
        let h = scaler.y(bh) as u32;

        // Clamp to image bounds
        let x = x.min(image.width().saturating_sub(1));
        let y = y.min(image.height().saturating_sub(1));
        let w = w.min(image.width().saturating_sub(x));
        let h = h.min(image.height().saturating_sub(y));

        if w == 0 || h == 0 {
            return Ok(String::new());
        }

        let sub = image.view(x, y, w, h).to_image();
        let text = self.ocr_model.image_to_text(&sub, false)?;
        Ok(text.trim().to_string())
    }

    /// Scan a single weapon from a captured game image.
    ///
    /// Port of `scanSingleWeapon()` from GOODScanner/lib/weapon_scanner.js
    fn scan_single_weapon(&self, image: &RgbImage, scaler: &CoordScaler) -> Result<WeaponScanResult> {
        // OCR weapon name
        let name_text = self.ocr_image_region(image, self.ocr_regions.name, scaler)?;
        let weapon_key = fuzzy_match_map(&name_text, &self.mappings.weapon_name_map);

        if weapon_key.is_none() {
            // Check if it's a stop-signal weapon/material
            for &stop_name in WEAPON_STOP_NAMES.iter().chain(WEAPON_FORGING_STOP_NAMES.iter()) {
                if name_text.contains(stop_name) {
                    info!("[weapon] detected \u{300C}{}\u{300D}, stopping", stop_name);
                    return Ok(WeaponScanResult::Stop);
                }
            }

            if pixel_utils::detect_weapon_rarity(image, scaler) <= 2 {
                info!("[weapon] detected low-star item, stopping");
                return Ok(WeaponScanResult::Stop);
            }

            if self.config.continue_on_failure {
                warn!("[weapon] cannot match: \u{300C}{}\u{300D}, skipping", name_text);
                return Ok(WeaponScanResult::Skip);
            }
            bail!("Cannot match weapon: \u{300C}{}\u{300D}", name_text);
        }

        let weapon_key = weapon_key.unwrap();

        // OCR level
        let level_text = self.ocr_image_region(image, self.ocr_regions.level, scaler)?;
        let (level, ascended) = Self::parse_weapon_level(&level_text);

        // OCR refinement
        let ref_text = self.ocr_image_region(image, self.ocr_regions.refinement, scaler)?;
        let refinement = Self::parse_refinement(&ref_text);

        // OCR equip status
        let equip_text = self.ocr_image_region(image, self.ocr_regions.equip, scaler)?;
        let location = self.parse_equip_location(&equip_text);

        // Pixel-based detections
        let rarity = pixel_utils::detect_weapon_rarity(image, scaler);
        let lock = pixel_utils::detect_weapon_lock(image, scaler);
        let ascension = level_to_ascension(level, ascended);

        Ok(WeaponScanResult::Weapon(GoodWeapon {
            key: weapon_key,
            level,
            ascension,
            refinement,
            rarity,
            location,
            lock,
        }))
    }

    /// Parse weapon level from "XX/YY" or "Lv.X" format.
    /// Returns (level, ascended).
    fn parse_weapon_level(text: &str) -> (i32, bool) {
        if text.is_empty() {
            return (1, false);
        }

        let slash_re = Regex::new(r"(\d+)\s*/\s*(\d+)").unwrap();
        if let Some(caps) = slash_re.captures(text) {
            let level: i32 = caps[1].parse().unwrap_or(1);
            let raw_max: i32 = caps[2].parse().unwrap_or(20);
            let max_level = ((raw_max as f64 / 10.0).round() * 10.0) as i32;
            let ascended = level >= 20 && level < max_level;
            return (level, ascended);
        }

        let lv_re = Regex::new(r"(?i)[Ll][Vv]\.?\s*(\d+)").unwrap();
        if let Some(caps) = lv_re.captures(text) {
            let level: i32 = caps[1].parse().unwrap_or(1);
            return (level, false);
        }

        let level = navigation::parse_number_from_text(text);
        (if level > 0 { level } else { 1 }, false)
    }

    /// Parse refinement from text.
    /// Tries: "精炼X" → "RX" → bare digit 1-5.
    ///
    /// Port of refinement parsing from weapon_scanner.js
    fn parse_refinement(text: &str) -> i32 {
        if text.is_empty() {
            return 1;
        }

        // Try "精炼X"
        let cn_re = Regex::new(r"\u{7CBE}\u{70BC}\s*(\d)").unwrap();
        if let Some(caps) = cn_re.captures(text) {
            return caps[1].parse().unwrap_or(1);
        }

        // Try "RX"
        let r_re = Regex::new(r"(?i)[Rr](\d)").unwrap();
        if let Some(caps) = r_re.captures(text) {
            return caps[1].parse().unwrap_or(1);
        }

        // Try bare digit
        let d_re = Regex::new(r"(\d)").unwrap();
        if let Some(caps) = d_re.captures(text) {
            let d: i32 = caps[1].parse().unwrap_or(0);
            if (1..=5).contains(&d) {
                return d;
            }
        }

        1
    }

    /// Parse equipped character from equip text.
    /// Text format: "已装备: CharacterName" or similar.
    fn parse_equip_location(&self, text: &str) -> String {
        // "已装备" = "Equipped"
        if text.contains("\u{5DF2}\u{88C5}\u{5907}") {
            let char_name = text
                .replace("\u{5DF2}\u{88C5}\u{5907}", "")
                .replace([':', '\u{FF1A}', ' '], "")
                .trim()
                .to_string();
            if !char_name.is_empty() {
                return fuzzy_match_map(&char_name, &self.mappings.character_name_map)
                    .unwrap_or_default();
            }
        }
        String::new()
    }

    /// Scan all weapons from the backpack.
    ///
    /// Uses `BackpackScanner` for grid traversal with panel-load detection
    /// and adaptive scrolling. The controller is passed in to avoid borrow
    /// conflicts between BackpackScanner and the scan callback.
    ///
    /// If `start_at > 0`, skips directly to that item index.
    pub fn scan(
        &self,
        ctrl: &mut GenshinGameController,
        skip_open_backpack: bool,
        start_at: usize,
    ) -> Result<Vec<GoodWeapon>> {
        info!("[weapon] starting scan...");
        let now = SystemTime::now();

        let mut bp = BackpackScanner::new(ctrl);

        if !skip_open_backpack {
            bp.open_backpack(self.config.open_delay);
        }
        bp.select_tab("weapon", self.config.delay_tab);

        // Read item count
        let (_, total_count) = bp.read_item_count(self.ocr_model.as_ref())?;

        if total_count == 0 {
            warn!("[weapon] no weapons in backpack");
            return Ok(Vec::new());
        }
        info!("[weapon] total: {}", total_count);

        let mut weapons: Vec<GoodWeapon> = Vec::new();

        let scan_config = BackpackScanConfig {
            delay_grid_item: self.config.delay_grid_item,
            delay_scroll: self.config.delay_scroll,
        };

        // Clone the scaler so the callback can use it without borrowing ctrl
        let scaler = bp.scaler().clone();

        // Row-level duplicate detection: if the same row of items appears again
        // (and is not all lv1), scrolling has failed and we should stop.
        let mut seen_rows: Vec<String> = Vec::new();
        let mut current_row: Vec<String> = Vec::new();
        let mut pending_row: Vec<GoodWeapon> = Vec::new();
        let mut row_has_leveled = false;

        bp.scan_grid(
            total_count as usize,
            &scan_config,
            start_at,
            |event| {
                match event {
                    GridEvent::PageScrolled => {
                        seen_rows.clear();
                        current_row.clear();
                        pending_row.clear();
                        row_has_leveled = false;
                        return ScanAction::Continue;
                    }
                    GridEvent::Item(_, image) => {
                        match self.scan_single_weapon(image, &scaler) {
                            Ok(WeaponScanResult::Weapon(weapon)) => {
                                let fp = format!("{}|{}|R{}", weapon.key, weapon.level, weapon.refinement);
                                if weapon.level > 1 {
                                    row_has_leveled = true;
                                }
                                current_row.push(fp);
                                if weapon.rarity >= self.config.min_rarity {
                                    pending_row.push(weapon);
                                }
                            }
                            Ok(WeaponScanResult::Stop) => {
                                // Flush pending row before stopping
                                for w in pending_row.drain(..) {
                                    if self.config.log_progress {
                                        info!(
                                            "[weapon] {} Lv.{} R{} {}{}",
                                            w.key, w.level, w.refinement,
                                            if w.location.is_empty() { "-" } else { &w.location },
                                            if w.lock { " locked" } else { "" }
                                        );
                                    }
                                    weapons.push(w);
                                }
                                return ScanAction::Stop;
                            }
                            Ok(WeaponScanResult::Skip) => {
                                current_row.push("skip".to_string());
                            }
                            Err(e) => {
                                error!("[weapon] scan error: {}", e);
                                current_row.push("error".to_string());
                                if !self.config.continue_on_failure {
                                    return ScanAction::Stop;
                                }
                            }
                        }

                        // Check row when full
                        if current_row.len() >= GRID_COLS {
                            let row_str = current_row.join(",");
                            let is_dup = row_has_leveled && seen_rows.iter().any(|s| s == &row_str);

                            if is_dup {
                                warn!(
                                    "[weapon] duplicate row detected (scroll failed?), stopping. Row: {}",
                                    row_str
                                );
                                return ScanAction::Stop;
                            }

                            seen_rows.push(row_str);
                            for w in pending_row.drain(..) {
                                if self.config.log_progress {
                                    info!(
                                        "[weapon] {} Lv.{} R{} {}{}",
                                        w.key, w.level, w.refinement,
                                        if w.location.is_empty() { "-" } else { &w.location },
                                        if w.lock { " locked" } else { "" }
                                    );
                                }
                                weapons.push(w);
                            }
                            current_row.clear();
                            pending_row.clear();
                            row_has_leveled = false;
                        }

                        ScanAction::Continue
                    }
                }
            },
        );

        // Flush remaining partial row
        for w in pending_row.drain(..) {
            if self.config.log_progress {
                info!(
                    "[weapon] {} Lv.{} R{} {}{}",
                    w.key, w.level, w.refinement,
                    if w.location.is_empty() { "-" } else { &w.location },
                    if w.lock { " locked" } else { "" }
                );
            }
            weapons.push(w);
        }

        info!(
            "[weapon] complete, {} weapons scanned in {:?}",
            weapons.len(),
            now.elapsed().unwrap_or_default()
        );

        Ok(weapons)
    }

    /// Debug scan a single weapon from a captured image.
    ///
    /// Returns detailed per-field OCR results including raw text, parsed values,
    /// and timing information. Used by the re-scan debug mode.
    pub fn debug_scan_single(
        &self,
        image: &RgbImage,
        scaler: &CoordScaler,
    ) -> DebugScanResult {
        use std::time::Instant;

        let total_start = Instant::now();
        let mut fields = Vec::new();

        // Name
        let t = Instant::now();
        let name_text = self.ocr_image_region(image, self.ocr_regions.name, scaler)
            .unwrap_or_default();
        let name_key = fuzzy_match_map(&name_text, &self.mappings.weapon_name_map)
            .unwrap_or_default();
        fields.push(DebugOcrField {
            field_name: "name".into(),
            raw_text: name_text,
            parsed_value: name_key.clone(),
            region: self.ocr_regions.name,
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Level
        let t = Instant::now();
        let level_text = self.ocr_image_region(image, self.ocr_regions.level, scaler)
            .unwrap_or_default();
        let (level, ascended) = Self::parse_weapon_level(&level_text);
        fields.push(DebugOcrField {
            field_name: "level".into(),
            raw_text: level_text,
            parsed_value: format!("lv={} ascended={}", level, ascended),
            region: self.ocr_regions.level,
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Refinement
        let t = Instant::now();
        let ref_text = self.ocr_image_region(image, self.ocr_regions.refinement, scaler)
            .unwrap_or_default();
        let refinement = Self::parse_refinement(&ref_text);
        fields.push(DebugOcrField {
            field_name: "refinement".into(),
            raw_text: ref_text,
            parsed_value: format!("R{}", refinement),
            region: self.ocr_regions.refinement,
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Equip
        let t = Instant::now();
        let equip_text = self.ocr_image_region(image, self.ocr_regions.equip, scaler)
            .unwrap_or_default();
        let location = self.parse_equip_location(&equip_text);
        fields.push(DebugOcrField {
            field_name: "equip".into(),
            raw_text: equip_text,
            parsed_value: if location.is_empty() { "(none)".into() } else { location.clone() },
            region: self.ocr_regions.equip,
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Pixel detections (not OCR but still timed)
        let t = Instant::now();
        let rarity = pixel_utils::detect_weapon_rarity(image, scaler);
        let lock = pixel_utils::detect_weapon_lock(image, scaler);
        let ascension = level_to_ascension(level, ascended);
        fields.push(DebugOcrField {
            field_name: "pixel_detect".into(),
            raw_text: String::new(),
            parsed_value: format!("rarity={} lock={} ascension={}", rarity, lock, ascension),
            region: (0.0, 0.0, 0.0, 0.0),
            duration_ms: t.elapsed().as_millis() as u64,
        });

        let weapon = GoodWeapon {
            key: name_key,
            level,
            ascension,
            refinement,
            rarity,
            location,
            lock,
        };
        let parsed_json = serde_json::to_string_pretty(&weapon).unwrap_or_default();

        DebugScanResult {
            fields,
            total_duration_ms: total_start.elapsed().as_millis() as u64,
            parsed_json,
        }
    }
}
