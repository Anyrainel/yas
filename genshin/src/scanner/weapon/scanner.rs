use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{bail, Result};
use image::{GenericImageView, RgbImage};
use log::{debug, error, info, warn};
use regex::Regex;

use yas::ocr::ImageToText;

use super::GoodWeaponScannerConfig;
use crate::scanner::common::backpack_scanner::{BackpackScanConfig, BackpackScanner, GridEvent, ScanAction};
use crate::scanner::common::constants::*;
use crate::scanner::common::coord_scaler::CoordScaler;
use crate::scanner::common::equip_parser;
use crate::scanner::common::fuzzy_match::fuzzy_match_map;

lazy_static::lazy_static! {
    static ref SLASH_RE: Regex = Regex::new(r"(\d+)\s*/\s*(\d+)").unwrap();
    static ref LV_RE: Regex = Regex::new(r"(?i)[Ll][Vv]\.?\s*(\d+)").unwrap();
    static ref REFINE_CN_RE: Regex = Regex::new(r"\u{7CBE}\u{70BC}\s*(\d)").unwrap();
    static ref REFINE_R_RE: Regex = Regex::new(r"(?i)[Rr](\d)").unwrap();
    static ref BARE_DIGIT_RE: Regex = Regex::new(r"(\d)").unwrap();
}
use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::MappingManager;
use crate::scanner::common::models::{DebugOcrField, DebugScanResult, GoodWeapon};
use crate::scanner::common::ocr_factory;
use crate::scanner::common::ocr_pool::OcrPool;
use crate::scanner::common::pixel_utils;
use crate::scanner::common::scan_worker::{self, WorkItem};
use crate::scanner::common::stat_parser::level_to_ascension;

/// OCR regions for weapon card (at 1920x1080 base).
///
/// Computed from card proportions (card origin 1307,119, size 494x841).
/// The proportional approach adapts better to the actual card layout than
/// the hardcoded constants from the JS port.
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
            name: (1337.0, 121.0, 434.0, 55.0),
            level: (
                card_x + (card_w * 0.060).round(),
                card_y + (card_h * 0.367).round(),
                (card_w * 0.272).round(),
                (card_h * 0.035).round(),
            ),
            refinement: (
                card_x + (card_w * 0.058).round(),
                card_y + (card_h * 0.417).round(),
                (card_w * 0.30).round(),
                (card_h * 0.038).round(),
            ),
            // Equip text "CharName已装备" at bottom of card area.
            // Narrowed to skip avatar icon on left and excess space on right.
            equip: (1386.0, 905.0, 315.0, 50.0),
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
    mappings: Arc<MappingManager>,
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
        mappings: Arc<MappingManager>,
    ) -> Result<Self> {
        Ok(Self {
            config,
            mappings,
            ocr_regions: WeaponOcrRegions::new(),
        })
    }
}

impl GoodWeaponScanner {
    /// OCR a sub-region of a captured game image.
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

    /// Scan a single weapon from a captured game image.
    ///
    /// Called from the worker thread with a checked-out OCR model.
    fn scan_single_weapon(
        ocr: &dyn ImageToText<RgbImage>,
        equip_fallback_ocr: Option<&dyn ImageToText<RgbImage>>,
        image: &RgbImage,
        scaler: &CoordScaler,
        ocr_regions: &WeaponOcrRegions,
        mappings: &MappingManager,
        config: &GoodWeaponScannerConfig,
        item_index: usize,
    ) -> Result<WeaponScanResult> {
        use crate::scanner::common::debug_dump::DumpCtx;
        use super::super::common::constants::{WEAPON_LOCK_POS1, STAR_Y};

        // OCR weapon name
        let name_text = Self::ocr_image_region(ocr, image, ocr_regions.name, scaler)?;
        let weapon_key = fuzzy_match_map(&name_text, &mappings.weapon_name_map);
        if config.verbose {
            debug!("[weapon] 名称OCR: {:?} -> {:?} / [weapon] name OCR: {:?} -> {:?}", name_text, weapon_key, name_text, weapon_key);
        }

        if weapon_key.is_none() {
            // Check if it's a stop-signal weapon/material
            for &stop_name in WEAPON_STOP_NAMES.iter().chain(WEAPON_FORGING_STOP_NAMES.iter()) {
                if name_text.contains(stop_name) {
                    info!("[weapon] 检测到「{}」，停止扫描 / [weapon] detected 「{}」, stopping", stop_name, stop_name);
                    return Ok(WeaponScanResult::Stop);
                }
            }

            if pixel_utils::detect_weapon_rarity(image, scaler) <= 2 {
                info!("[weapon] 检测到低星级物品，停止扫描 / [weapon] detected low-star item, stopping");
                return Ok(WeaponScanResult::Stop);
            }

            if config.continue_on_failure {
                warn!("[weapon] 无法匹配「{}」，跳过 / [weapon] cannot match: 「{}」, skipping", name_text, name_text);
                return Ok(WeaponScanResult::Skip);
            }
            bail!("无法匹配武器 / Cannot match weapon: \u{300C}{}\u{300D}", name_text);
        }

        let weapon_key = weapon_key.unwrap();

        // OCR level
        let level_text = Self::ocr_image_region(ocr, image, ocr_regions.level, scaler)?;
        let (level, ascended) = Self::parse_weapon_level(&level_text);

        // OCR refinement
        let ref_text = Self::ocr_image_region(ocr, image, ocr_regions.refinement, scaler)?;
        let refinement = Self::parse_refinement(&ref_text);

        // OCR equip status
        // Try primary engine first, fall back to v5 if no match (v4 dict lacks rare chars like 魈)
        let equip_text = Self::ocr_image_region(ocr, image, ocr_regions.equip, scaler)?;
        let mut location = Self::parse_equip_location(&equip_text, mappings);
        if location.is_empty() && equip_text.trim().len() >= 2 {
            if let Some(fallback) = equip_fallback_ocr {
                let equip_text_v5 = Self::ocr_image_region(fallback, image, ocr_regions.equip, scaler)?;
                location = Self::parse_equip_location(&equip_text_v5, mappings);
                if !location.is_empty() {
                    debug!("[weapon] {} 装备: v4「{}」失败, v5「{}」→ {} / [weapon] {} equip: v4「{}」failed, v5「{}」→ {}", weapon_key, equip_text.trim(), equip_text_v5.trim(), location, weapon_key, equip_text.trim(), equip_text_v5.trim(), location);
                }
            }
        }
        if !equip_text.is_empty() {
            debug!("[weapon] {} 装备OCR: {:?} -> {:?} / [weapon] {} equip OCR: {:?} -> {:?}", weapon_key, equip_text, location, weapon_key, equip_text, location);
        }

        // Pixel-based detections
        let rarity = pixel_utils::detect_weapon_rarity(image, scaler);
        let lock = pixel_utils::detect_weapon_lock(image, scaler);
        let ascension = level_to_ascension(level, ascended);

        // Dump all OCR and pixel-check regions
        if config.dump_images {
            let ctx = DumpCtx::new("debug_images", "weapons", item_index, &weapon_key);
            ctx.dump_full(image);
            ctx.dump_region("name", image, ocr_regions.name, scaler);
            ctx.dump_region("level", image, ocr_regions.level, scaler);
            ctx.dump_region("refinement", image, ocr_regions.refinement, scaler);
            ctx.dump_region("equip", image, ocr_regions.equip, scaler);
            ctx.dump_pixel("star5_px", image, (1485.0, STAR_Y), 10, scaler);
            ctx.dump_pixel("star4_px", image, (1450.0, STAR_Y), 10, scaler);
            ctx.dump_pixel("star3_px", image, (1416.0, STAR_Y), 10, scaler);
            ctx.dump_pixel("lock_px", image, (WEAPON_LOCK_POS1.0, WEAPON_LOCK_POS1.1), 10, scaler);
        }

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

    /// Valid level caps for weapons.
    const VALID_MAX_LEVELS: &'static [i32] = &[20, 40, 50, 60, 70, 80, 90];

    /// Snap a raw max-level value to the nearest valid cap.
    fn snap_max_level(raw: i32) -> i32 {
        Self::VALID_MAX_LEVELS
            .iter()
            .copied()
            .min_by_key(|&v| (v - raw).unsigned_abs())
            .unwrap_or(raw)
    }

    /// Try to split a digit string into (level, max) pair.
    fn try_split_digits(digits: &str) -> Option<(i32, i32)> {
        for i in (1..digits.len()).rev() {
            if let (Ok(lv), Ok(mx)) = (digits[..i].parse::<i32>(), digits[i..].parse::<i32>()) {
                if (1..=90).contains(&lv) && (10..=90).contains(&mx) && mx >= lv {
                    return Some((lv, mx));
                }
            }
        }
        None
    }

    /// Parse weapon level from "XX/YY" or "Lv.X" format.
    /// Returns (level, ascended).
    ///
    /// Uses the same multi-stage parsing as character level:
    /// 1. Try "digits/digits" with slash
    /// 2. Extract all digits, try split
    /// 3. Remove one noise char and retry (OCR drops "/" → concatenated digits)
    /// 4. Partial extract (level only)
    fn parse_weapon_level(text: &str) -> (i32, bool) {
        if text.is_empty() {
            return (1, false);
        }

        // Phase 0: "XX/YY" with explicit slash
        if let Some(caps) = SLASH_RE.captures(text) {
            let level: i32 = caps[1].parse().unwrap_or(1);
            let raw_max: i32 = caps[2].parse().unwrap_or(20);
            let max_level = Self::snap_max_level(raw_max);
            let ascended = level >= 20 && level < max_level;
            return (level, ascended);
        }

        // Extract all digit chars
        let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return (1, false);
        }

        // Phase 1: direct split of all digits
        if let Some((lv, mx)) = Self::try_split_digits(&digits) {
            let max_level = Self::snap_max_level(mx);
            let ascended = lv >= 20 && lv < max_level;
            debug!("[weapon] 等级OCR直接分割: {:?} -> {}/{} / [weapon] level OCR direct split: {:?} -> {}/{}", text, lv, max_level, text, lv, max_level);
            return (lv, ascended);
        }

        // Phase 2: remove one noise char at each position, prefer mid-string
        {
            let mid = digits.len() as f64 / 2.0;
            let mut best: Option<(i32, i32, usize, f64)> = None;
            for remove_idx in 0..digits.len() {
                let reduced: String = digits
                    .char_indices()
                    .filter(|&(i, _)| i != remove_idx)
                    .map(|(_, c)| c)
                    .collect();
                if let Some((lv, mx)) = Self::try_split_digits(&reduced) {
                    let dist = (remove_idx as f64 - mid).abs();
                    if best.is_none() || dist < best.unwrap().3 {
                        best = Some((lv, mx, remove_idx, dist));
                    }
                }
            }
            if let Some((lv, mx, idx, _)) = best {
                let max_level = Self::snap_max_level(mx);
                let ascended = lv >= 20 && lv < max_level;
                debug!("[weapon] 等级OCR去噪: {:?} (移除索引 {}) -> {}/{} / [weapon] level OCR noise-remove: {:?} (rm idx {}) -> {}/{}", text, idx, lv, max_level, text, idx, lv, max_level);
                return (lv, ascended);
            }
        }

        // Phase 3: partial extract — just the level number
        if let Some(caps) = LV_RE.captures(text) {
            let level: i32 = caps[1].parse().unwrap_or(1);
            debug!("[weapon] 等级OCR部分匹配(Lv): {:?} -> {} / [weapon] level OCR partial (Lv): {:?} -> {}", text, level, text, level);
            return (level, false);
        }

        let level: i32 = digits.parse().unwrap_or(0);
        if (1..=90).contains(&level) {
            debug!("[weapon] 等级OCR纯数字: {:?} -> {} / [weapon] level OCR bare digits: {:?} -> {}", text, level, text, level);
            return (level, false);
        }

        warn!("[weapon] 等级OCR失败: {:?} / [weapon] level OCR failed: {:?}", text, text);
        (1, false)
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
        if let Some(caps) = REFINE_CN_RE.captures(text) {
            return caps[1].parse().unwrap_or(1);
        }

        // Try "RX"
        if let Some(caps) = REFINE_R_RE.captures(text) {
            return caps[1].parse().unwrap_or(1);
        }

        // Try bare digit
        if let Some(caps) = BARE_DIGIT_RE.captures(text) {
            let d: i32 = caps[1].parse().unwrap_or(0);
            if (1..=5).contains(&d) {
                return d;
            }
        }

        1
    }

    fn parse_equip_location(text: &str, mappings: &MappingManager) -> String {
        equip_parser::parse_equip_location(text, &mappings.character_name_map)
    }

    /// Scan all weapons from the backpack.
    ///
    /// Uses pipelined architecture: the main thread navigates the grid and
    /// captures screenshots, while a worker pool OCRs them in parallel.
    ///
    /// If `start_at > 0`, skips directly to that item index.
    pub fn scan(
        &self,
        ctrl: &mut GenshinGameController,
        skip_open_backpack: bool,
        start_at: usize,
    ) -> Result<Vec<GoodWeapon>> {
        debug!("[weapon] 开始扫描... / [weapon] starting scan...");
        let now = SystemTime::now();

        if !skip_open_backpack {
            // Return to main world using BGI-style strategy:
            // press Escape one at a time, verify after each press.
            ctrl.focus_game_window();
            ctrl.return_to_main_ui(8);
        }

        let mut bp = BackpackScanner::new(ctrl);

        if !skip_open_backpack {
            bp.open_backpack(self.config.open_delay);
        }
        bp.select_tab("weapon", self.config.delay_tab);

        // Create a temporary OCR model just for reading item count
        let count_ocr = ocr_factory::create_ocr_model(&self.config.ocr_backend)?;
        let (current_count, _max_capacity) = bp.read_item_count(count_ocr.as_ref())?;

        // If count is 0, try reopening backpack
        let total_count = if current_count == 0 {
            info!("[weapon] 数量=0，重新打开背包... / [weapon] count=0, reopening backpack...");
            drop(bp);
            ctrl.return_to_main_ui(4);
            let mut bp2 = BackpackScanner::new(ctrl);
            bp2.open_backpack(self.config.open_delay);
            bp2.select_tab("weapon", self.config.delay_tab);
            let (count, _) = bp2.read_item_count(count_ocr.as_ref())?;
            drop(bp2);
            bp = BackpackScanner::new(ctrl);
            count
        } else {
            current_count
        };

        if total_count == 0 {
            info!("[weapon] 背包中无武器 / [weapon] no weapons in backpack");
            return Ok(Vec::new());
        }

        let total_count = if self.config.max_count > 0 {
            let capped = (total_count as usize).min(self.config.max_count + start_at) as i32;
            info!("[weapon] 总计: {} (受max_count={}限制，截取为{}) / [weapon] total: {} (capped to {} by max_count={})", total_count, self.config.max_count, capped, total_count, capped, self.config.max_count);
            capped
        } else {
            debug!("[weapon] 总计: {} / [weapon] total: {}", total_count, total_count);
            total_count
        };

        let scaler = bp.scaler().clone();

        // Create OCR pools.
        // Weapon scanner uses a single engine (v4) for all fields.
        // 4 instances for parallel scanning.
        // 1 v5 instance for equip fallback (v4 dict lacks rare chars like 魈/Xiao).
        let ocr_backend = self.config.ocr_backend.clone();
        let ocr_pool = Arc::new(OcrPool::new(
            move || ocr_factory::create_ocr_model(&ocr_backend),
            4,
        )?);
        let equip_fallback_pool = Arc::new(OcrPool::new(
            || ocr_factory::create_ocr_model("ppocrv5"),
            1,
        )?);
        debug!("[weapon] OCR池: v4=4, 装备回退(v5)=1 / [weapon] OCR pool: v4=4, equip_fallback(v5)=1");

        // Shared context for worker threads
        let worker_mappings = self.mappings.clone();
        let worker_config = self.config.clone();
        let worker_scaler = scaler.clone();
        let worker_ocr_pool = ocr_pool.clone();
        let worker_equip_pool = equip_fallback_pool.clone();
        let worker_ocr_regions = WeaponOcrRegions::new();

        let (item_tx, worker_handle) = scan_worker::start_worker::<(), GoodWeapon, _>(
            total_count as usize,
            move |work_item: WorkItem<()>| {
                let ocr_guard = worker_ocr_pool.get();
                let equip_guard = worker_equip_pool.get();

                match Self::scan_single_weapon(
                    &ocr_guard,
                    Some(&equip_guard as &dyn ImageToText<RgbImage>),
                    &work_item.image,
                    &worker_scaler,
                    &worker_ocr_regions,
                    &worker_mappings,
                    &worker_config,
                    work_item.index,
                )? {
                    WeaponScanResult::Weapon(weapon) => {
                        if weapon.rarity >= worker_config.min_rarity {
                            Ok(Some(weapon))
                        } else {
                            Ok(None)
                        }
                    }
                    WeaponScanResult::Stop => Ok(None),
                    WeaponScanResult::Skip => Ok(None),
                }
            },
        );

        let scan_config = BackpackScanConfig {
            delay_grid_item: self.config.delay_grid_item,
            delay_scroll: self.config.delay_scroll,
            delay_after_panel: if self.config.skip_lock_delay { 0 } else { self.config.delay_grid_item },
        };

        bp.scan_grid(
            total_count as usize,
            &scan_config,
            start_at,
            |image| pixel_utils::is_weapon_icon_ambiguous(image, &scaler),
            |event| {
                match event {
                    GridEvent::PageScrolled => ScanAction::Continue,
                    GridEvent::Item(idx, image) => {
                        if worker_handle.stop_requested() {
                            return ScanAction::Stop;
                        }

                        // Quick rarity check on main thread
                        let rarity = pixel_utils::detect_weapon_rarity(&image, &scaler);
                        if rarity <= 2 {
                            debug!("[weapon] 检测到{}星物品，停止捕获 / [weapon] detected {}* item, stopping capture", rarity, rarity);
                            return ScanAction::Stop;
                        }

                        // Normalize index to 0-based so the worker's BTreeMap drain works correctly.
                        let worker_idx = idx - start_at;
                        if item_tx.send(WorkItem { index: worker_idx, image, metadata: () }).is_err() {
                            error!("[weapon] 工作通道已关闭 / [weapon] worker channel closed");
                            return ScanAction::Stop;
                        }

                        ScanAction::Continue
                    }
                }
            },
        );

        drop(item_tx);
        let weapons = worker_handle.join();

        info!(
            "[weapon] 完成，扫描了{}把武器，耗时{:?} / [weapon] complete, {} weapons scanned in {:?}",
            weapons.len(),
            now.elapsed().unwrap_or_default(),
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
        ocr: &dyn ImageToText<RgbImage>,
        image: &RgbImage,
        scaler: &CoordScaler,
    ) -> DebugScanResult {
        use std::time::Instant;

        let total_start = Instant::now();
        let mut fields = Vec::new();

        // Name
        let t = Instant::now();
        let name_text = Self::ocr_image_region(ocr, image, self.ocr_regions.name, scaler)
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
        let level_text = Self::ocr_image_region(ocr, image, self.ocr_regions.level, scaler)
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
        let ref_text = Self::ocr_image_region(ocr, image, self.ocr_regions.refinement, scaler)
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
        let equip_text = Self::ocr_image_region(ocr, image, self.ocr_regions.equip, scaler)
            .unwrap_or_default();
        let location = Self::parse_equip_location(&equip_text, &self.mappings);
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
