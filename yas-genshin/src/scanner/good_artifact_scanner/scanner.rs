use std::rc::Rc;
use std::time::SystemTime;

use anyhow::{bail, Result};
use image::{GenericImageView, RgbImage};
use log::{error, info, warn};
use regex::Regex;

use yas::ocr::ImageToText;

use super::GoodArtifactScannerConfig;
use crate::scanner::good_common::backpack_scanner::{BackpackScanConfig, BackpackScanner, GridEvent, ScanAction};
use crate::scanner::good_common::constants::*;
use crate::scanner::good_common::coord_scaler::CoordScaler;
use crate::scanner::good_common::fuzzy_match::fuzzy_match_map;
use crate::scanner::good_common::game_controller::GenshinGameController;
use crate::scanner::good_common::mappings::MappingManager;
use crate::scanner::good_common::models::{DebugOcrField, DebugScanResult, GoodArtifact, GoodSubStat};
use crate::scanner::good_common::ocr_factory;
use crate::scanner::good_common::pixel_utils;
use crate::scanner::good_common::stat_parser;

/// Computed OCR regions for artifact card (at 1920x1080 base).
///
/// Coordinates derived from the old window_info JSON at 2560x1440, scaled by 0.75.
struct ArtifactOcrRegions {
    part_name: (f64, f64, f64, f64),
    main_stat: (f64, f64, f64, f64),
    level: (f64, f64, f64, f64),
    /// Per-line substat regions: (x, y, w, h) for each of the 4 possible substats.
    /// Heights increase for lower lines to match the game's actual layout.
    substat_lines: [(f64, f64, f64, f64); 4],
    set_name_x: f64,
    set_name_w: f64,
    set_name_base_y: f64,
    set_name_h: f64,
    equip: (f64, f64, f64, f64),
    elixir: (f64, f64, f64, f64),
}

impl ArtifactOcrRegions {
    fn new() -> Self {
        let card_x: f64 = 1307.0;
        let card_y: f64 = 119.0;
        let card_w: f64 = 494.0;
        let card_h: f64 = 841.0;

        // Substat regions derived from old window_info (2560x1440 * 0.75):
        //   left=1356, width=255
        //   line 1: top=478, h=35
        //   line 2: top=513, h=37
        //   line 3: top=550, h=39
        //   line 4: top=589, h=39
        let sub_x = 1356.0;
        let sub_w = 255.0;

        Self {
            part_name: (
                card_x + (card_w * 0.0405).round(),
                card_y + (card_h * 0.0772).round(),
                (card_w * 0.4757).round(),
                (card_h * 0.0475).round(),
            ),
            main_stat: (
                card_x + (card_w * 0.0405).round(),
                card_y + (card_h * 0.1722).round(),
                (card_w * 0.4555).round(),
                (card_h * 0.0416).round(),
            ),
            level: (
                card_x + (card_w * 0.0506).round(),
                card_y + (card_h * 0.3634).round(),
                (card_w * 0.1417).round(),
                (card_h * 0.0416).round(),
            ),
            substat_lines: [
                (sub_x, 478.0, sub_w, 35.0),
                (sub_x, 513.0, sub_w, 37.0),
                (sub_x, 550.0, sub_w, 39.0),
                (sub_x, 589.0, sub_w, 39.0),
            ],
            set_name_x: 1330.0,
            set_name_w: 280.0,
            set_name_base_y: 625.0,
            set_name_h: 45.0,
            equip: (
                card_x + (card_w * 0.10).round(),
                card_y + (card_h * 0.935).round(),
                (card_w * 0.85).round(),
                (card_h * 0.06).round(),
            ),
            elixir: (1360.0, 410.0, 140.0, 26.0),
        }
    }
}

/// Result of scanning a single artifact
enum ArtifactScanResult {
    Artifact(GoodArtifact),
    Stop,
    Skip,
}

/// Artifact scanner ported from GOODScanner/lib/artifact_scanner.js.
///
/// Features elixir detection with Y-shift, astral marks, unactivated substats,
/// row-level deduplication, and post-processing filters.
///
/// The scanner holds only business logic (OCR model, mappings, config).
/// The game controller is passed to `scan()` to avoid borrow checker conflicts
/// with `BackpackScanner`.
pub struct GoodArtifactScanner {
    config: GoodArtifactScannerConfig,
    ocr_model: Box<dyn ImageToText<RgbImage> + Send>,
    mappings: Rc<MappingManager>,
    ocr_regions: ArtifactOcrRegions,
}

impl GoodArtifactScanner {
    pub fn new(
        config: GoodArtifactScannerConfig,
        mappings: Rc<MappingManager>,
    ) -> Result<Self> {
        let ocr_model = ocr_factory::create_ocr_model(&config.ocr_backend)?;

        Ok(Self {
            config,
            ocr_model,
            mappings,
            ocr_regions: ArtifactOcrRegions::new(),
        })
    }
}

impl GoodArtifactScanner {
    /// OCR a sub-region of a captured game image.
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

    /// OCR a sub-region after converting to high-contrast grayscale.
    /// Uses Otsu-like adaptive thresholding to produce clear black text on
    /// white background, which helps with colored text (green set names).
    fn ocr_image_region_grayscale(
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

        let x = x.min(image.width().saturating_sub(1));
        let y = y.min(image.height().saturating_sub(1));
        let w = w.min(image.width().saturating_sub(x));
        let h = h.min(image.height().saturating_sub(y));

        if w == 0 || h == 0 {
            return Ok(String::new());
        }

        let sub = image.view(x, y, w, h).to_image();

        // Convert to grayscale and compute min/max for adaptive threshold
        let mut gray_vals: Vec<u8> = Vec::with_capacity((sub.width() * sub.height()) as usize);
        let gray_img = RgbImage::from_fn(sub.width(), sub.height(), |px, py| {
            let p = sub.get_pixel(px, py);
            let g = (0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64) as u8;
            gray_vals.push(g);
            image::Rgb([g, g, g])
        });

        // Try simple grayscale first
        let text_gray = self.ocr_model.image_to_text(&gray_img, false)?;
        let text_gray = text_gray.trim().to_string();
        if self.find_set_key_in_text(&text_gray).is_some() {
            return Ok(text_gray);
        }

        // Green-channel extraction: the set name text is green (high G, low R/B).
        // Extract green saturation: G - max(R, B). Text pixels will have high values.
        // Then invert to get dark text on white background.
        let green_extracted = RgbImage::from_fn(sub.width(), sub.height(), |px, py| {
            let p = sub.get_pixel(px, py);
            let r = p[0] as i32;
            let g = p[1] as i32;
            let b = p[2] as i32;
            let green_excess = (g - r.max(b)).max(0);
            // Invert: high green_excess (text) → dark, low → light
            let v = (255 - (green_excess * 4).min(255)) as u8;
            image::Rgb([v, v, v])
        });
        let text_green = self.ocr_model.image_to_text(&green_extracted, false)?;
        let text_green = text_green.trim().to_string();
        if self.find_set_key_in_text(&text_green).is_some() {
            return Ok(text_green);
        }

        // Return whichever has more Chinese characters
        let cn = |s: &str| s.chars().filter(|c| *c >= '\u{4E00}' && *c <= '\u{9FFF}').count();
        let best = [text_gray, text_green].into_iter()
            .max_by_key(|s| cn(s))
            .unwrap_or_default();
        Ok(best)
    }

    /// OCR a sub-region with Y-offset and left-side icon masking.
    /// Replaces the leftmost ~18 pixels of the cropped image with the
    /// average background color to remove stat icons that confuse OCR.
    fn ocr_image_region_shifted_masked(
        &self,
        image: &RgbImage,
        rect: (f64, f64, f64, f64),
        y_shift: f64,
        scaler: &CoordScaler,
    ) -> Result<String> {
        let (bx, by, bw, bh) = rect;
        let x = scaler.x(bx) as u32;
        let y = scaler.y(by + y_shift) as u32;
        let w = scaler.x(bw) as u32;
        let h = scaler.y(bh) as u32;

        let x = x.min(image.width().saturating_sub(1));
        let y = y.min(image.height().saturating_sub(1));
        let w = w.min(image.width().saturating_sub(x));
        let h = h.min(image.height().saturating_sub(y));

        if w == 0 || h == 0 {
            return Ok(String::new());
        }

        let mut sub = image.view(x, y, w, h).to_image();

        // Mask the first ~18 pixels (stat icon area) with background color.
        // Sample background color from the right side of the image.
        let mask_width = 18u32.min(w);
        let sample_x = (w * 3 / 4).min(w.saturating_sub(1));
        let bg_color = if h > 0 {
            // Average a few pixels from the right side
            let mut r_sum = 0u32;
            let mut g_sum = 0u32;
            let mut b_sum = 0u32;
            let mut count = 0u32;
            for sy in [0, h / 2, h.saturating_sub(1)] {
                let p = sub.get_pixel(sample_x, sy);
                r_sum += p[0] as u32;
                g_sum += p[1] as u32;
                b_sum += p[2] as u32;
                count += 1;
            }
            image::Rgb([(r_sum / count) as u8, (g_sum / count) as u8, (b_sum / count) as u8])
        } else {
            image::Rgb([0, 0, 0])
        };

        for px in 0..mask_width {
            for py in 0..h {
                sub.put_pixel(px, py, bg_color);
            }
        }

        let text = self.ocr_model.image_to_text(&sub, false)?;
        Ok(text.trim().to_string())
    }

    /// OCR just the right portion (number area) of a substat line.
    /// Used as a retry when the full line OCR truncates the decimal value.
    /// By cropping to just the number, each character gets more pixels in the
    /// model's fixed-width input, improving decimal point recognition.
    fn ocr_substat_number_retry(
        &self,
        image: &RgbImage,
        rect: (f64, f64, f64, f64),
        y_shift: f64,
        scaler: &CoordScaler,
    ) -> Result<String> {
        let (bx, by, bw, bh) = rect;
        // Crop to right 60% of the line (stat value + % area)
        let num_x = bx + bw * 0.40;
        let num_w = bw * 0.60;
        let x = scaler.x(num_x) as u32;
        let y = scaler.y(by + y_shift) as u32;
        let w = scaler.x(num_w) as u32;
        let h = scaler.y(bh) as u32;

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

    /// OCR a sub-region with Y-offset for elixir artifacts.
    fn ocr_image_region_shifted(
        &self,
        image: &RgbImage,
        rect: (f64, f64, f64, f64),
        y_shift: f64,
        scaler: &CoordScaler,
    ) -> Result<String> {
        let (x, y, w, h) = rect;
        self.ocr_image_region(image, (x, y + y_shift, w, h), scaler)
    }

    /// Find artifact set key in OCR text (with multi-line fallback).
    ///
    /// Port of `findSetKeyInText()` from artifact_scanner.js
    fn find_set_key_in_text(&self, text: &str) -> Option<String> {
        if text.is_empty() {
            return None;
        }

        // Strip trailing punctuation that the OCR picks up from the set description
        // (e.g., "风起之日：" → "风起之日")
        let cleaned = text
            .trim()
            .trim_end_matches('：')
            .trim_end_matches(':')
            .trim_end_matches('；')
            .trim_end_matches(';')
            .trim();

        // Try cleaned text first
        if let Some(key) = fuzzy_match_map(cleaned, &self.mappings.artifact_set_map) {
            return Some(key);
        }

        // Try full text (in case cleaning removed something needed)
        if cleaned != text.trim() {
            if let Some(key) = fuzzy_match_map(text.trim(), &self.mappings.artifact_set_map) {
                return Some(key);
            }
        }

        // Try each line (for multi-line OCR results)
        for line in text.split('\n') {
            let line = line.trim()
                .trim_end_matches('：')
                .trim_end_matches(':')
                .trim();
            if line.len() < 2 {
                continue;
            }
            if let Some(key) = fuzzy_match_map(line, &self.mappings.artifact_set_map) {
                return Some(key);
            }
        }

        None
    }

    /// Detect elixir crafted status from OCR.
    fn detect_elixir_crafted(
        &self,
        image: &RgbImage,
        scaler: &CoordScaler,
    ) -> Result<bool> {
        let text = self.ocr_image_region(image, self.ocr_regions.elixir, scaler)?;
        // "祝圣" = elixir
        Ok(text.contains("\u{795D}\u{5723}"))
    }

    /// Parse equipped character from equip text.
    fn parse_equip_location(&self, text: &str) -> String {
        if text.contains("\u{5DF2}\u{88C5}\u{5907}") {
            // "已装备"
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

    /// Scan a single artifact from a captured game image.
    ///
    /// Port of `scanSingleArtifact()` from GOODScanner/lib/artifact_scanner.js
    fn scan_single_artifact(
        &self,
        image: &RgbImage,
        scaler: &CoordScaler,
    ) -> Result<ArtifactScanResult> {
        // 0. Detect rarity — stop on 3-star or below
        let rarity = pixel_utils::detect_artifact_rarity(image, scaler);
        if rarity <= 3 {
            info!("[artifact] detected {}* item, stopping", rarity);
            return Ok(ArtifactScanResult::Stop);
        }

        // 1. Part name → slot key
        let part_text = self.ocr_image_region(image, self.ocr_regions.part_name, scaler)?;
        let slot_key = stat_parser::match_slot_key(&part_text);

        let slot_key = match slot_key {
            Some(k) => k.to_string(),
            None => {
                // 4-star with unrecognizable slot = possibly elixir essence, skip
                if rarity == 4 {
                    info!("[artifact] 4* unrecognizable slot (possibly elixir essence), skipping");
                    return Ok(ArtifactScanResult::Skip);
                }
                if self.config.continue_on_failure {
                    warn!("[artifact] cannot identify slot: \u{300C}{}\u{300D}, skipping", part_text);
                    return Ok(ArtifactScanResult::Skip);
                }
                bail!("Cannot identify artifact slot: \u{300C}{}\u{300D}", part_text);
            }
        };

        // 2. Main stat
        let main_stat_text = self.ocr_image_region(image, self.ocr_regions.main_stat, scaler)?;
        let main_stat_key = if slot_key == "flower" {
            Some("hp".to_string())
        } else if slot_key == "plume" {
            Some("atk".to_string())
        } else {
            // For sands/goblet/circlet, HP/ATK/DEF are always percent.
            // The main stat OCR region only captures the name (not the value with "%"),
            // so we need to fix up flat→percent.
            stat_parser::parse_stat_from_text(&main_stat_text)
                .map(|s| stat_parser::main_stat_key_fixup(&s.key))
        };

        let main_stat_key = match main_stat_key {
            Some(k) => k,
            None => {
                if self.config.continue_on_failure {
                    warn!("[artifact] cannot identify main stat: \u{300C}{}\u{300D}, skipping", main_stat_text);
                    return Ok(ArtifactScanResult::Skip);
                }
                bail!("Cannot identify main stat: \u{300C}{}\u{300D}", main_stat_text);
            }
        };

        // 3. Detect elixir crafted
        let elixir_crafted = self.detect_elixir_crafted(image, scaler)?;
        let y_shift = if elixir_crafted { ELIXIR_SHIFT } else { 0.0 };

        // 4. Level
        let level_text = self.ocr_image_region_shifted(image, self.ocr_regions.level, y_shift, scaler)?;
        let level = {
            let re = Regex::new(r"\+?\s*(\d+)").unwrap();
            re.captures(&level_text)
                .and_then(|c| c[1].parse::<i32>().ok())
                .unwrap_or(0)
        };

        // 5. Substats — read each line individually (PaddleOCR is single-line)
        let mut substats: Vec<GoodSubStat> = Vec::new();
        let mut unactivated_substats: Vec<GoodSubStat> = Vec::new();
        for i in 0..4 {
            let (sub_x, sub_y, sub_w, sub_h) = self.ocr_regions.substat_lines[i];
            // Try regular OCR first; if unparseable, retry with icon masking.
            let line_text = {
                let text = self.ocr_image_region_shifted(
                    image, (sub_x, sub_y, sub_w, sub_h), y_shift, scaler,
                )?;
                if stat_parser::parse_stat_from_text(&text).is_some() {
                    text
                } else {
                    // Regular OCR failed — try with left-side icon masking
                    let masked = self.ocr_image_region_shifted_masked(
                        image, (sub_x, sub_y, sub_w, sub_h), y_shift, scaler,
                    )?;
                    if stat_parser::parse_stat_from_text(&masked).is_some() {
                        masked
                    } else {
                        // Neither worked — return the one with more Chinese chars
                        let cn = |s: &str| s.chars().filter(|c| *c >= '\u{4E00}' && *c <= '\u{9FFF}').count();
                        if cn(&masked) > cn(&text) { masked } else { text }
                    }
                }
            };
            let line = line_text.trim();
            if self.config.verbose {
                info!("[artifact] sub[{}] y={:.0} text=「{}」", i, sub_y + y_shift, line);
            }
            if line.len() < 2 {
                continue;
            }
            // Stop at "2件套" marker
            if line.contains("2\u{4EF6}\u{5957}") {
                break;
            }
            if let Some(mut parsed) = stat_parser::parse_stat_from_text(line) {
                // Retry on truncated percent values: OCR often drops digit after decimal.
                // Only retry when there's evidence of truncation: "X.%" (dot directly before %)
                // or "X.letter%" (OCR-corrupted digit after dot).
                let has_truncation_evidence = line.contains(".%")
                    || Regex::new(r"\.\D%").map_or(false, |re| re.is_match(line));
                if parsed.key.ends_with('_') && has_truncation_evidence {
                    // OCR just the number portion for better decimal recognition
                    if let Ok(num_text) = self.ocr_substat_number_retry(
                        image, (sub_x, sub_y, sub_w, sub_h), y_shift, scaler,
                    ) {
                        let num_text = num_text.trim();
                        // Extract number from the retry text
                        if let Some(retry_val) = stat_parser::extract_number(num_text) {
                            // Accept retry if: has decimal part, within same magnitude as original,
                            // and the integer part is close (within ±1 or same).
                            let has_decimal = retry_val != (retry_val as i64) as f64;
                            let orig_int = parsed.value as i64;
                            let retry_int = retry_val as i64;
                            // The retry crops the right portion, so it might see "1.7" from "11.7"
                            // Accept if retry integer part matches original, or if original was
                            // wrong and retry captures the last digit(s).
                            let close_enough = (retry_int - orig_int).abs() <= 1
                                || retry_val > (parsed.value * 0.8) && retry_val < (parsed.value * 1.3);
                            if has_decimal && retry_val > 0.5 && retry_val < 100.0 && close_enough {
                                info!("[artifact] sub[{}] decimal recovered: {} → {} (retry=「{}」)", i, parsed.value, retry_val, num_text);
                                parsed.value = retry_val;
                            }
                        }
                    }
                    if parsed.value == (parsed.value as i64) as f64 {
                        warn!("[artifact] sub[{}] truncation? key={} val={} raw=「{}」", i, parsed.key, parsed.value, line);
                    }
                }
                let sub = GoodSubStat {
                    key: parsed.key,
                    value: parsed.value,
                };
                // Check if inactive: rely on OCR detecting "待激活" text.
                // Pixel brightness detection was too unreliable (326+ false positives),
                // so we use only the OCR text and level-0 inference (below).
                let is_inactive = parsed.inactive;
                if is_inactive {
                    unactivated_substats.push(sub);
                } else {
                    substats.push(sub);
                }
            } else {
                warn!("[artifact] sub[{}] unparseable: 「{}」", i, line);
            }
        }

        // Note: level-0 inference was removed. The groundtruth treats level-0
        // artifacts with 4 substats as all-active, so we shouldn't infer
        // unactivated status from level alone.

        // 6. Set name — try multiple Y positions since OCR may miss some substats.
        //    The set name line is below the substats, so its Y depends on how many
        //    substats exist. Rather than relying on the parsed count (which may
        //    undercount due to OCR errors), try the 4-substat position first,
        //    then fall back to 3, 2, 1 positions.
        let stat_count = (substats.len() + unactivated_substats.len()).clamp(1, 4);
        if stat_count < 4 && rarity == 5 && self.config.verbose {
            warn!("[artifact] 5* only identified {} substats", stat_count);
        }

        let mut set_key: Option<String> = None;
        let mut set_name_text = String::new();
        let mut tried_y = 0.0;

        // Try from 4 substats down to 1 (most common case first)
        for assumed_count in (1..=4).rev() {
            let missing = 4 - assumed_count;
            let set_y = self.ocr_regions.set_name_base_y + y_shift - (missing as f64 * 40.0);
            let set_rect = (self.ocr_regions.set_name_x, set_y, self.ocr_regions.set_name_w, self.ocr_regions.set_name_h);
            // Try regular OCR first, then grayscale fallback.
            // Some set names work better with one or the other.
            let text_rgb = self.ocr_image_region(image, set_rect, scaler)?;
            let text = if self.find_set_key_in_text(&text_rgb).is_some() {
                text_rgb
            } else {
                let text_gray = self.ocr_image_region_grayscale(image, set_rect, scaler)?;
                if self.find_set_key_in_text(&text_gray).is_some() {
                    text_gray
                } else {
                    // Neither matched — use whichever has more Chinese characters
                    let cn_count = |s: &str| s.chars().filter(|c| *c >= '\u{4E00}' && *c <= '\u{9FFF}').count();
                    if cn_count(&text_rgb) >= cn_count(&text_gray) { text_rgb } else { text_gray }
                }
            };
            if self.config.verbose {
                info!(
                    "[artifact] set probe: assumed_count={} set_y={:.0} text=「{}」",
                    assumed_count, set_y, text
                );
            }
            if let Some(key) = self.find_set_key_in_text(&text) {
                set_key = Some(key);
                set_name_text = text;
                tried_y = set_y;
                break;
            }
            if set_name_text.is_empty() {
                set_name_text = text;
                tried_y = set_y;
            }
        }

        let set_key = match set_key {
            Some(k) => k,
            None => {
                let stat_keys: Vec<String> = substats
                    .iter()
                    .map(|s| s.key.clone())
                    .chain(unactivated_substats.iter().map(|s| format!("{}(inactive)", s.key)))
                    .collect();
                warn!(
                    "[artifact] cannot identify set: setY={} stats=[{}] text=\u{300C}{}\u{300D}",
                    tried_y,
                    stat_keys.join(", "),
                    set_name_text
                );
                if self.config.continue_on_failure {
                    return Ok(ArtifactScanResult::Skip);
                }
                bail!(
                    "Cannot identify artifact set (substats={}): \u{300C}{}\u{300D}",
                    stat_count,
                    set_name_text
                );
            }
        };

        // 8. Equipped character
        let equip_text = self.ocr_image_region(image, self.ocr_regions.equip, scaler)?;
        let location = self.parse_equip_location(&equip_text);

        // 9. Lock
        let lock = pixel_utils::detect_artifact_lock(image, scaler, y_shift);

        // 10. Astral mark
        let astral_mark = pixel_utils::detect_artifact_astral_mark(image, scaler, y_shift);

        Ok(ArtifactScanResult::Artifact(GoodArtifact {
            set_key,
            slot_key,
            level,
            rarity,
            main_stat_key,
            substats,
            location,
            lock,
            astral_mark,
            elixir_crafted,
            unactivated_substats,
        }))
    }

    /// Generate a fingerprint for row-level deduplication.
    fn artifact_fingerprint(artifact: &GoodArtifact) -> String {
        let subs: Vec<String> = artifact
            .substats
            .iter()
            .map(|s| format!("{}:{}", s.key, s.value))
            .collect();
        format!(
            "{}|{}|{}|{}|{}|{}",
            artifact.set_key,
            artifact.slot_key,
            artifact.level,
            artifact.main_stat_key,
            artifact.rarity,
            subs.join(";")
        )
    }

    /// Scan all artifacts from the backpack.
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
    ) -> Result<Vec<GoodArtifact>> {
        info!("[artifact] starting scan...");
        let now = SystemTime::now();

        // Focus the game window before doing anything
        ctrl.focus_game_window();

        // Press Escape to close any open menus before starting
        ctrl.key_press(enigo::Key::Escape);
        yas::utils::sleep(500);

        let mut bp = BackpackScanner::new(ctrl);

        if !skip_open_backpack {
            bp.open_backpack(self.config.open_delay);
        }
        bp.select_tab("artifact", self.config.delay_tab);

        let (_, total_count) = bp.read_item_count(self.ocr_model.as_ref())?;

        if total_count == 0 {
            warn!("[artifact] no artifacts in backpack");
            return Ok(Vec::new());
        }
        info!("[artifact] total: {}", total_count);

        let mut artifacts: Vec<GoodArtifact> = Vec::new();
        let mut fail_count = 0;

        // Row-level deduplication
        let mut seen_rows: Vec<String> = Vec::new();
        let mut current_row: Vec<String> = Vec::new();
        let mut pending_row: Vec<GoodArtifact> = Vec::new();

        let scan_config = BackpackScanConfig {
            delay_grid_item: self.config.delay_grid_item,
            delay_scroll: self.config.delay_scroll,
        };

        // Clone scaler so callback doesn't conflict with BackpackScanner's borrow
        let scaler = bp.scaler().clone();

        bp.scan_grid(
            total_count as usize,
            &scan_config,
            start_at,
            |event| {
                match event {
                    GridEvent::PageScrolled => {
                        // Clear row cache on page scroll
                        seen_rows.clear();
                        current_row.clear();
                        pending_row.clear();
                        return ScanAction::Continue;
                    }
                    GridEvent::Item(_idx, image) => {
                        match self.scan_single_artifact(image, &scaler) {
                            Ok(ArtifactScanResult::Artifact(artifact)) => {
                                let fingerprint = Self::artifact_fingerprint(&artifact);
                                current_row.push(fingerprint);
                                if artifact.rarity >= self.config.min_rarity {
                                    pending_row.push(artifact);
                                    fail_count = 0;
                                }
                            }
                            Ok(ArtifactScanResult::Stop) => {
                                return ScanAction::Stop;
                            }
                            Ok(ArtifactScanResult::Skip) => {
                                current_row.push("skip".to_string());
                                fail_count = 0;
                            }
                            Err(e) => {
                                error!("[artifact] scan error: {}", e);
                                current_row.push("null".to_string());
                                if !self.config.continue_on_failure {
                                    return ScanAction::Stop;
                                }
                                fail_count += 1;
                            }
                        }

                        // Row full → check deduplication
                        if current_row.len() >= GRID_COLS {
                            let row_str = current_row.join(",");
                            let is_dup = seen_rows.iter().any(|s| s == &row_str);

                            if is_dup {
                                warn!("[artifact] detected duplicate row, skipping {} items", pending_row.len());
                            } else {
                                seen_rows.push(row_str);
                                for a in pending_row.drain(..) {
                                    if self.config.log_progress {
                                        info!(
                                            "[artifact] {} {} +{} {}* {}{}{}",
                                            a.set_key, a.slot_key, a.level, a.rarity,
                                            if a.location.is_empty() { "-" } else { &a.location },
                                            if a.lock { " locked" } else { "" },
                                            if a.elixir_crafted { " elixir" } else { "" },
                                        );
                                    }
                                    artifacts.push(a);
                                }
                                fail_count = 0;
                            }
                            current_row.clear();
                            pending_row.clear();
                        }

                        if fail_count >= 10 {
                            error!("[artifact] {} consecutive failures, stopping", fail_count);
                            return ScanAction::Stop;
                        }

                        ScanAction::Continue
                    }
                }
            },
        );

        // Flush partial final row
        if !current_row.is_empty() {
            let row_str = current_row.join(",");
            let is_dup = seen_rows.iter().any(|s| s == &row_str);
            if !is_dup {
                for a in pending_row.drain(..) {
                    if self.config.log_progress {
                        info!(
                            "[artifact] {} {} +{} {}* {}{}{}",
                            a.set_key, a.slot_key, a.level, a.rarity,
                            if a.location.is_empty() { "-" } else { &a.location },
                            if a.lock { " locked" } else { "" },
                            if a.elixir_crafted { " elixir" } else { "" },
                        );
                    }
                    artifacts.push(a);
                }
            }
        }

        // Post-processing: remove unleveled 4-star artifacts from 5-star-capable sets
        let before_count = artifacts.len();
        artifacts.retain(|a| {
            if a.rarity == 4 && a.level == 0 {
                if let Some(&max_rarity) = self.mappings.artifact_set_max_rarity.get(&a.set_key) {
                    if max_rarity >= 5 {
                        return false;
                    }
                }
            }
            true
        });
        if artifacts.len() < before_count {
            info!(
                "[artifact] filtered {} unleveled 4* low-value artifacts",
                before_count - artifacts.len()
            );
        }

        info!(
            "[artifact] complete, {} artifacts scanned (>={}*) in {:?}",
            artifacts.len(),
            self.config.min_rarity,
            now.elapsed().unwrap_or_default()
        );

        Ok(artifacts)
    }

    /// Debug scan a single artifact from a captured image.
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

        // Rarity (pixel)
        let t = Instant::now();
        let rarity = pixel_utils::detect_artifact_rarity(image, scaler);
        fields.push(DebugOcrField {
            field_name: "rarity".into(),
            raw_text: String::new(),
            parsed_value: format!("{}*", rarity),
            region: (0.0, 0.0, 0.0, 0.0),
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Part name → slot key
        let t = Instant::now();
        let part_text = self.ocr_image_region(image, self.ocr_regions.part_name, scaler)
            .unwrap_or_default();
        let slot_key = stat_parser::match_slot_key(&part_text)
            .map(|s| s.to_string())
            .unwrap_or_default();
        fields.push(DebugOcrField {
            field_name: "slot".into(),
            raw_text: part_text,
            parsed_value: slot_key.clone(),
            region: self.ocr_regions.part_name,
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Main stat
        let t = Instant::now();
        let main_stat_text = self.ocr_image_region(image, self.ocr_regions.main_stat, scaler)
            .unwrap_or_default();
        let main_stat_key = if slot_key == "flower" {
            "hp".to_string()
        } else if slot_key == "plume" {
            "atk".to_string()
        } else {
            stat_parser::parse_stat_from_text(&main_stat_text)
                .map(|s| stat_parser::main_stat_key_fixup(&s.key))
                .unwrap_or_default()
        };
        fields.push(DebugOcrField {
            field_name: "mainStat".into(),
            raw_text: main_stat_text,
            parsed_value: main_stat_key.clone(),
            region: self.ocr_regions.main_stat,
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Elixir detection
        let t = Instant::now();
        let elixir_crafted = self.detect_elixir_crafted(image, scaler).unwrap_or(false);
        let y_shift = if elixir_crafted { ELIXIR_SHIFT } else { 0.0 };
        fields.push(DebugOcrField {
            field_name: "elixir".into(),
            raw_text: String::new(),
            parsed_value: format!("{}", elixir_crafted),
            region: self.ocr_regions.elixir,
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Level
        let t = Instant::now();
        let level_text = self.ocr_image_region_shifted(image, self.ocr_regions.level, y_shift, scaler)
            .unwrap_or_default();
        let level = {
            let re = Regex::new(r"\+?\s*(\d+)").unwrap();
            re.captures(&level_text)
                .and_then(|c| c[1].parse::<i32>().ok())
                .unwrap_or(0)
        };
        fields.push(DebugOcrField {
            field_name: "level".into(),
            raw_text: level_text,
            parsed_value: format!("+{}", level),
            region: self.ocr_regions.level,
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Substats — read each line individually
        let t = Instant::now();
        let mut substats: Vec<GoodSubStat> = Vec::new();
        let mut unactivated_substats: Vec<GoodSubStat> = Vec::new();
        let mut subs_raw_lines = Vec::new();
        for i in 0..4 {
            let (sub_x, sub_y, sub_w, sub_h) = self.ocr_regions.substat_lines[i];
            let line_text = self.ocr_image_region_shifted(
                image, (sub_x, sub_y, sub_w, sub_h), y_shift, scaler,
            ).unwrap_or_default();
            let line = line_text.trim().to_string();
            if line.len() < 2 { subs_raw_lines.push(line); continue; }
            if line.contains("2\u{4EF6}\u{5957}") { break; }
            if let Some(parsed) = stat_parser::parse_stat_from_text(&line) {
                let sub = GoodSubStat { key: parsed.key, value: parsed.value };
                if parsed.inactive {
                    unactivated_substats.push(sub);
                } else {
                    substats.push(sub);
                }
            }
            subs_raw_lines.push(line);
        }
        // Level-0 inference removed — groundtruth treats all as active
        let subs_summary: Vec<String> = substats.iter()
            .map(|s| format!("{}={}", s.key, s.value))
            .chain(unactivated_substats.iter().map(|s| format!("{}={}(inactive)", s.key, s.value)))
            .collect();
        fields.push(DebugOcrField {
            field_name: "substats".into(),
            raw_text: subs_raw_lines.join(" | "),
            parsed_value: subs_summary.join(", "),
            region: self.ocr_regions.substat_lines[0],
            duration_ms: t.elapsed().as_millis() as u64,
        });

        // Set name
        let t = Instant::now();
        let stat_count = (substats.len() + unactivated_substats.len()).clamp(1, 4);
        let missing_stats = 4 - stat_count as i32;
        let set_y = self.ocr_regions.set_name_base_y + y_shift - (missing_stats as f64 * 40.0);
        let set_rect = (self.ocr_regions.set_name_x, set_y, self.ocr_regions.set_name_w, self.ocr_regions.set_name_h);
        let set_name_text = {
            let rgb = self.ocr_image_region(image, set_rect, scaler).unwrap_or_default();
            if self.find_set_key_in_text(&rgb).is_some() {
                rgb
            } else {
                let gray = self.ocr_image_region_grayscale(image, set_rect, scaler).unwrap_or_default();
                if self.find_set_key_in_text(&gray).is_some() { gray } else { rgb }
            }
        };
        let set_key = self.find_set_key_in_text(&set_name_text).unwrap_or_default();
        fields.push(DebugOcrField {
            field_name: "setName".into(),
            raw_text: set_name_text,
            parsed_value: set_key.clone(),
            region: set_rect,
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

        // Lock + astral mark (pixel)
        let t = Instant::now();
        let lock = pixel_utils::detect_artifact_lock(image, scaler, y_shift);
        let astral_mark = pixel_utils::detect_artifact_astral_mark(image, scaler, y_shift);
        fields.push(DebugOcrField {
            field_name: "pixel_detect".into(),
            raw_text: String::new(),
            parsed_value: format!("lock={} astral={}", lock, astral_mark),
            region: (0.0, 0.0, 0.0, 0.0),
            duration_ms: t.elapsed().as_millis() as u64,
        });

        let artifact = GoodArtifact {
            set_key,
            slot_key,
            level,
            rarity,
            main_stat_key,
            substats,
            location,
            lock,
            astral_mark,
            elixir_crafted,
            unactivated_substats,
        };
        let parsed_json = serde_json::to_string_pretty(&artifact).unwrap_or_default();

        DebugScanResult {
            fields,
            total_duration_ms: total_start.elapsed().as_millis() as u64,
            parsed_json,
        }
    }
}
