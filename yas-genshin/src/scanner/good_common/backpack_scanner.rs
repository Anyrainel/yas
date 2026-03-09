use anyhow::Result;
use image::RgbImage;
use log::{error, info, warn};
use regex::Regex;

use yas::ocr::ImageToText;
use yas::utils;

use super::constants::*;
use super::game_controller::GenshinGameController;

/// What the scan callback should do after processing an item.
pub enum ScanAction {
    /// Continue scanning.
    Continue,
    /// Stop scanning immediately.
    Stop,
}

/// Events delivered to the scan callback.
pub enum GridEvent<'a> {
    /// An item was clicked and captured: (item_index, captured_image).
    Item(usize, &'a RgbImage),
    /// A page scroll just completed (useful for clearing per-page state).
    PageScrolled,
}

/// Configuration for backpack grid scanning delays.
pub struct BackpackScanConfig {
    pub delay_grid_item: u64,
    pub delay_scroll: u64,
}

/// Panel pool rect — region of the detail panel whose pixel sum changes
/// when a different item is selected.
const PANEL_POOL_RECT: (f64, f64, f64, f64) = (1400.0, 300.0, 300.0, 200.0);

/// Default timeout for panel-load detection (milliseconds).
const PANEL_LOAD_TIMEOUT_MS: u64 = 400;

/// Delay between scroll ticks (milliseconds).
const SCROLL_TICK_DELAY_MS: u32 = 10;

/// Wait time after all scroll ticks are sent, for animation to settle.
const SCROLL_SETTLE_MS: u32 = 200;

/// Reusable backpack grid scanner.
///
/// Uses pre-calibrated scroll constants (SCROLL_TICKS_PER_PAGE) for reliable
/// page-level scrolling. Each page scroll sends exactly SCROLL_TICKS_PER_PAGE
/// ticks, with a correction tick subtracted every SCROLL_CORRECTION_INTERVAL
/// pages to prevent drift.
pub struct BackpackScanner<'a> {
    ctrl: &'a mut GenshinGameController,
    /// Number of pages scrolled so far (for correction tracking).
    pages_scrolled: u32,
}

impl<'a> BackpackScanner<'a> {
    pub fn new(ctrl: &'a mut GenshinGameController) -> Self {
        Self {
            ctrl,
            pages_scrolled: 0,
        }
    }

    /// Access the controller's scaler (useful for cloning before scan_grid).
    pub fn scaler(&self) -> &super::coord_scaler::CoordScaler {
        &self.ctrl.scaler
    }

    /// Open the backpack by pressing B.
    /// Assumes the game is on the main overworld UI.
    pub fn open_backpack(&mut self, delay: u64) {
        self.ctrl.key_press(enigo::Key::Layout('b'));
        utils::sleep(delay as u32);
    }

    /// Select a backpack tab by clicking its position.
    pub fn select_tab(&mut self, tab: &str, delay: u64) {
        let (bx, by) = match tab {
            "weapon" => TAB_WEAPON,
            "artifact" => TAB_ARTIFACT,
            _ => {
                error!("[backpack] unknown tab: {}", tab);
                return;
            }
        };
        self.ctrl.click_at(bx, by);
        utils::sleep(delay as u32);
    }

    /// Read the item count from the backpack header ("X/Y" format).
    pub fn read_item_count(
        &self,
        ocr_model: &dyn ImageToText<RgbImage>,
    ) -> Result<(i32, i32)> {
        let text = self.ctrl.ocr_region(ocr_model, ITEM_COUNT_RECT)?;
        let re = Regex::new(r"(\d+)\s*/\s*(\d+)")?;
        if let Some(caps) = re.captures(&text) {
            let current: i32 = caps[1].parse().unwrap_or(0);
            let total: i32 = caps[2].parse().unwrap_or(0);
            Ok((current, total))
        } else {
            Ok((0, 0))
        }
    }

    /// Scroll down by a given number of rows using calibrated tick counts.
    ///
    /// Uses SCROLL_TICKS_PER_PAGE (49 ticks for 5 rows) as the base ratio.
    /// Applies correction every SCROLL_CORRECTION_INTERVAL pages.
    fn scroll_rows(&mut self, row_count: usize) -> bool {
        if row_count == 0 {
            return true;
        }

        // Move mouse to grid center for consistent scroll behavior
        let center_x = GRID_FIRST_X + 3.0 * GRID_OFFSET_X;
        let center_y = GRID_FIRST_Y + 2.0 * GRID_OFFSET_Y;
        self.ctrl.move_to(center_x, center_y);
        utils::sleep(30);

        // Calculate ticks: SCROLL_TICKS_PER_PAGE ticks per GRID_ROWS rows
        let ticks_per_row = SCROLL_TICKS_PER_PAGE as f64 / GRID_ROWS as f64;
        let mut ticks = (ticks_per_row * row_count as f64).round() as i32;

        // Apply correction for full-page scrolls
        if row_count == GRID_ROWS {
            self.pages_scrolled += 1;
            if SCROLL_CORRECTION_INTERVAL > 0
                && self.pages_scrolled % SCROLL_CORRECTION_INTERVAL as u32 == 0
            {
                ticks -= 1;
                info!(
                    "[backpack] scroll correction at page {} (-1 tick)",
                    self.pages_scrolled
                );
            }
        }

        info!(
            "[backpack] scroll {} rows ({} ticks, page {})",
            row_count, ticks, self.pages_scrolled
        );

        // Send scroll ticks with small delays to avoid overwhelming the game
        for i in 0..ticks {
            if utils::is_rmb_down() {
                return false;
            }
            self.ctrl.mouse_scroll(1);
            // Small delay between ticks
            if (i + 1) % 5 == 0 {
                utils::sleep(SCROLL_TICK_DELAY_MS);
            }
        }

        // Wait for scroll animation to settle
        utils::sleep(SCROLL_SETTLE_MS);
        true
    }

    /// Main grid traversal with panel-load detection.
    ///
    /// For each item: clicks the grid position, waits for panel to load
    /// (pixel pool detection), captures the game screen, and delivers a
    /// `GridEvent::Item` to the callback.
    ///
    /// After each page scroll, delivers `GridEvent::PageScrolled`.
    ///
    /// The callback returns `ScanAction::Continue` or `ScanAction::Stop`.
    pub fn scan_grid<F>(
        &mut self,
        total: usize,
        _config: &BackpackScanConfig,
        start_at: usize,
        mut callback: F,
    ) where
        F: FnMut(GridEvent) -> ScanAction,
    {
        let total_row = (total + GRID_COLS - 1) / GRID_COLS;
        let last_row_col = if total % GRID_COLS == 0 { GRID_COLS } else { total % GRID_COLS };

        info!(
            "[backpack] total={} items, {} rows, last row has {} items",
            total, total_row, last_row_col
        );

        // Click the first grid position to ensure focus
        self.ctrl.click_at(GRID_FIRST_X, GRID_FIRST_Y);
        utils::sleep(500);

        let row = GRID_ROWS.min(total_row);
        let mut scanned_row: usize = 0;
        let mut scanned_count: usize = 0;
        let mut start_row: usize = 0;

        // Skip pages by scrolling
        if start_at > 0 {
            let skip_rows = start_at / GRID_COLS;
            let full_pages = skip_rows / GRID_ROWS;
            if full_pages > 0 {
                info!(
                    "[backpack] jumping to item {} ({} rows to skip)",
                    start_at, skip_rows
                );
                let rows_to_scroll = full_pages * GRID_ROWS;
                if !self.scroll_rows(rows_to_scroll) {
                    return; // interrupted
                }
                scanned_row = rows_to_scroll;
                scanned_count = rows_to_scroll * GRID_COLS;
                utils::sleep(200);
            }
        }

        'outer: while scanned_count < total {
            for cur_row in start_row..row {
                let row_item_count = if scanned_row == total_row - 1 {
                    last_row_col
                } else {
                    GRID_COLS
                };

                for col in 0..row_item_count {
                    if utils::is_rmb_down() || scanned_count >= total {
                        break 'outer;
                    }

                    // Skip items before start_at
                    if scanned_count < start_at {
                        scanned_count += 1;
                        continue;
                    }

                    // Click the grid item
                    let x = GRID_FIRST_X + col as f64 * GRID_OFFSET_X;
                    let y = GRID_FIRST_Y + cur_row as f64 * GRID_OFFSET_Y;
                    self.ctrl.click_at(x, y);

                    // Wait for panel to load
                    let _ = self.ctrl.wait_until_panel_loaded(
                        PANEL_POOL_RECT,
                        PANEL_LOAD_TIMEOUT_MS,
                    );

                    // Capture and process
                    let image = match self.ctrl.capture_game() {
                        Ok(img) => img,
                        Err(e) => {
                            error!("[backpack] capture failed: {}", e);
                            scanned_count += 1;
                            continue;
                        }
                    };

                    match callback(GridEvent::Item(scanned_count, &image)) {
                        ScanAction::Continue => {}
                        ScanAction::Stop => break 'outer,
                    }

                    scanned_count += 1;
                }

                scanned_row += 1;
            }

            // Calculate how many rows remain and scroll
            let remain = total - scanned_count;
            if remain == 0 {
                break;
            }
            let remain_row = (remain + GRID_COLS - 1) / GRID_COLS;
            let scroll_row = remain_row.min(GRID_ROWS);
            start_row = GRID_ROWS - scroll_row;

            if !self.scroll_rows(scroll_row) {
                break 'outer;
            }

            callback(GridEvent::PageScrolled);
        }
    }
}
