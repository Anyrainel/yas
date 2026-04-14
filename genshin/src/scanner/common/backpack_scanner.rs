use anyhow::Result;
use image::RgbImage;
use log::{debug, error, info};
use regex::Regex;

use yas::ocr::ImageToText;
use yas::utils;

use super::constants::*;
use super::game_controller::GenshinGameController;
use super::pixel_utils;

/// If the "5-star sort by acquired time" filter is active on the artifact tab,
/// click it to dismiss so that all rarities are visible.
///
/// Must be called after the artifact tab is selected.
pub fn dismiss_five_star_filter(ctrl: &mut GenshinGameController, tab_delay: u64, dump_images: bool) {
    let image = match ctrl.capture_game() {
        Ok(img) => img,
        Err(e) => {
            log::warn!("[backpack] 截图失败，跳过筛选检测 / capture failed, skipping filter check: {}", e);
            return;
        }
    };
    if dump_images {
        use crate::scanner::common::debug_dump::DumpCtx;
        let ctx = DumpCtx::new("debug_images", "artifacts", 0, "filter_check");
        ctx.dump_full(&image);
        ctx.dump_pixel("five_star_filter_px", &image,
            (ARTIFACT_FIVE_STAR_FILTER_POS.0, ARTIFACT_FIVE_STAR_FILTER_POS.1), 10, &ctrl.scaler);
    }
    if pixel_utils::is_five_star_filter_active(&image, &ctrl.scaler) {
        ctrl.click_at(ARTIFACT_FIVE_STAR_FILTER_POS.0, ARTIFACT_FIVE_STAR_FILTER_POS.1);
        utils::sleep(tab_delay as u32);
    }
}

/// Open the backpack to a specific tab with the same proven sequence as the
/// artifact/weapon scanners.
///
/// 1. Focus game window
/// 2. Return to main world (Escape × 8)
/// 3. Press B to open backpack
/// 4. Click the requested tab
/// 5. Read item count; if 0, retry from step 2
///
/// Returns `(current_count, max_capacity)` on success.
///
/// This is a free function (not a method) so callers can use `ctrl` freely
/// after it returns without keeping a `BackpackScanner` borrow alive.
pub fn open_backpack_to_tab(
    ctrl: &mut GenshinGameController,
    tab: &str,
    open_delay: u64,
    tab_delay: u64,
    count_ocr: &dyn ImageToText<RgbImage>,
) -> Result<(i32, i32)> {
    ctrl.focus_game_window();
    if ctrl.check_rmb() {
        anyhow::bail!("cancelled");
    }
    ctrl.return_to_main_ui(8);
    if ctrl.check_rmb() {
        anyhow::bail!("cancelled");
    }

    {
        let mut bp = BackpackScanner::new(ctrl);
        bp.open_backpack(open_delay);
        bp.select_tab(tab, tab_delay);
    }

    // Dismiss the "5-star sort by acquired time" filter if active on the artifact tab.
    // When active, only 5-star artifacts are visible — we need all rarities.
    if tab == "artifact" {
        dismiss_five_star_filter(ctrl, tab_delay, false);
    }

    if ctrl.check_rmb() {
        anyhow::bail!("cancelled");
    }

    // Read item count (need a fresh BackpackScanner for the borrow)
    let (count, max) = {
        let bp = BackpackScanner::new(ctrl);
        bp.read_item_count(count_ocr)?
    };

    if count == 0 {
        info!("[backpack] 标签'{}'数量=0，重新打开背包... / [backpack] count=0 on tab '{}', reopening backpack...", tab, tab);
        if ctrl.check_rmb() {
            anyhow::bail!("cancelled");
        }
        ctrl.return_to_main_ui(4);
        if ctrl.check_rmb() {
            anyhow::bail!("cancelled");
        }
        {
            let mut bp = BackpackScanner::new(ctrl);
            bp.open_backpack(open_delay);
            bp.select_tab(tab, tab_delay);
        }
        // Check filter again after retry
        if tab == "artifact" {
            dismiss_five_star_filter(ctrl, tab_delay, false);
        }
        let bp = BackpackScanner::new(ctrl);
        return bp.read_item_count(count_ocr);
    }

    Ok((count, max))
}

/// What the scan callback should do after processing an event.
pub enum ScanAction {
    /// Continue scanning.
    Continue,
    /// Stop scanning immediately.
    Stop,
    /// Skip the rest of the current page. Only meaningful as a response to
    /// `PageStarted` — ignored (treated as `Continue`) if returned from other
    /// events.
    SkipPage,
}

/// Events delivered to the scan callback.
pub enum GridEvent {
    /// Fired at the start of each page if `probe_last_cell_per_page` is set.
    /// `scan_grid` has already clicked the bottom-right visible cell, waited
    /// for the panel, and captured the image. The callback may return
    /// `SkipPage` to skip the entire page without OCRing individual items.
    PageStarted {
        /// Cumulative item index at the top-left of the page.
        page_start_idx: usize,
        /// The captured image of the bottom-right visible cell.
        last_cell_image: RgbImage,
    },
    /// An item was clicked and captured. `row` and `col` are screen-relative
    /// grid positions (for re-clicking the same cell later).
    Item {
        idx: usize,
        row: usize,
        col: usize,
        image: RgbImage,
    },
    /// Fired after all items on a page have been processed, before scrolling.
    /// Only fires if at least one item was emitted on the page. Useful for
    /// draining per-page OCR results / applying side effects while the page
    /// is still in view.
    PageCompleted { page_start_idx: usize },
    /// A page scroll just completed (useful for clearing per-page state).
    PageScrolled,
}

/// Configuration for backpack grid scanning delays.
pub struct BackpackScanConfig {
    pub delay_scroll: u64,
    /// Delay (ms) after panel load detection, before capture.
    /// Gives the panel extra time to finish rendering text.
    pub delay_before_capture: u64,
    /// If true, `scan_grid` clicks the bottom-right visible cell at the start
    /// of each page, captures its image, and emits `GridEvent::PageStarted`
    /// so the caller can decide whether to skip the page. Scanners leave this
    /// false; the lock manager enables it for its level-based page-skip
    /// optimization.
    pub probe_last_cell_per_page: bool,
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
                error!("[backpack] 未知标签: {} / [backpack] unknown tab: {}", tab, tab);
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
                debug!(
                    "[backpack] 第{}页滚动修正(-1刻度) / [backpack] scroll correction at page {} (-1 tick)",
                    self.pages_scrolled, self.pages_scrolled
                );
            }
        }

        debug!(
            "[backpack] 滚动{}行({}刻度，第{}页) / [backpack] scroll {} rows ({} ticks, page {})",
            row_count, ticks, self.pages_scrolled, row_count, ticks, self.pages_scrolled
        );

        // Send scroll ticks with small delays to avoid overwhelming the game
        for i in 0..ticks {
            if self.ctrl.check_rmb() {
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
        config: &BackpackScanConfig,
        start_at: usize,
        mut callback: F,
    ) where
        F: FnMut(&mut GenshinGameController, GridEvent) -> ScanAction,
    {
        let total_row = (total + GRID_COLS - 1) / GRID_COLS;
        let last_row_col = if total % GRID_COLS == 0 { GRID_COLS } else { total % GRID_COLS };

        debug!(
            "[backpack] 总计={}个物品，{}行，最后一行有{}个 / [backpack] total={} items, {} rows, last row has {} items",
            total, total_row, last_row_col, total, total_row, last_row_col
        );

        // Click the first grid position to ensure focus
        self.ctrl.click_at(GRID_FIRST_X, GRID_FIRST_Y);
        utils::sleep(300);

        let row = GRID_ROWS.min(total_row);
        let mut scanned_row: usize = 0;
        let mut scanned_count: usize = 0;
        let mut start_row: usize = 0;

        // Skip pages by scrolling
        if start_at > 0 {
            let skip_rows = start_at / GRID_COLS;
            let full_pages = skip_rows / GRID_ROWS;
            if full_pages > 0 {
                debug!(
                    "[backpack] 跳转到第{}个物品(跳过{}行) / [backpack] jumping to item {} ({} rows to skip)",
                    start_at, skip_rows, start_at, skip_rows
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
            let page_start_idx = scanned_count;
            let mut page_had_items = false;
            let mut skip_page = false;

            // --- Optional page probe: click bottom-right new cell, capture, ask callback. ---
            if config.probe_last_cell_per_page {
                // Screen row of the bottom-most new row is always `row - 1`
                // (new rows occupy [start_row..row); bottom is row-1).
                let probe_screen_row = row - 1;
                let cum_row_at_probe = scanned_row + (probe_screen_row - start_row);
                let probe_col = if cum_row_at_probe == total_row - 1 {
                    last_row_col - 1
                } else {
                    GRID_COLS - 1
                };
                let x = GRID_FIRST_X + probe_col as f64 * GRID_OFFSET_X;
                let y = GRID_FIRST_Y + probe_screen_row as f64 * GRID_OFFSET_Y;
                self.ctrl.click_at(x, y);
                let _ = self.ctrl.wait_until_panel_loaded(
                    PANEL_POOL_RECT,
                    PANEL_LOAD_TIMEOUT_MS,
                );
                if config.delay_before_capture > 0 {
                    utils::sleep(config.delay_before_capture as u32);
                }
                match self.ctrl.capture_game() {
                    Ok(image) => {
                        let action = callback(
                            &mut *self.ctrl,
                            GridEvent::PageStarted {
                                page_start_idx,
                                last_cell_image: image,
                            },
                        );
                        match action {
                            ScanAction::Continue => {}
                            ScanAction::SkipPage => skip_page = true,
                            ScanAction::Stop => break 'outer,
                        }
                    }
                    Err(e) => {
                        error!(
                            "[backpack] 探测截图失败: {} / [backpack] probe capture failed: {}",
                            e, e
                        );
                    }
                }
            }

            if skip_page {
                // Advance counters past the skipped page without emitting items.
                let new_rows = row - start_row;
                let mut rows_added = 0usize;
                while rows_added < new_rows {
                    let cum_row = scanned_row + rows_added;
                    let row_item_count = if cum_row == total_row - 1 {
                        last_row_col
                    } else {
                        GRID_COLS
                    };
                    scanned_count += row_item_count;
                    rows_added += 1;
                    if scanned_count >= total {
                        break;
                    }
                }
                scanned_row += rows_added;
            } else {
                let mut stopped = false;
                'page: for cur_row in start_row..row {
                    let row_item_count = if scanned_row == total_row - 1 {
                        last_row_col
                    } else {
                        GRID_COLS
                    };

                    for col in 0..row_item_count {
                        if self.ctrl.check_rmb() || scanned_count >= total {
                            stopped = true;
                            break 'page;
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

                        // Pre-capture delay: let panel text finish rendering
                        if config.delay_before_capture > 0 {
                            utils::sleep(config.delay_before_capture as u32);
                        }

                        // Capture and process
                        let image = match self.ctrl.capture_game() {
                            Ok(img) => img,
                            Err(e) => {
                                error!("[backpack] 截图失败: {} / [backpack] capture failed: {}", e, e);
                                scanned_count += 1;
                                continue;
                            }
                        };

                        page_had_items = true;
                        let action = callback(
                            &mut *self.ctrl,
                            GridEvent::Item {
                                idx: scanned_count,
                                row: cur_row,
                                col,
                                image,
                            },
                        );
                        match action {
                            ScanAction::Continue => {}
                            ScanAction::Stop => {
                                scanned_count += 1;
                                stopped = true;
                                break 'page;
                            }
                            ScanAction::SkipPage => {
                                // SkipPage is only valid from PageStarted.
                                log::debug!(
                                    "[backpack] Item处理返回SkipPage，已忽略，视为Continue / SkipPage from Item handler is ignored; treating as Continue"
                                );
                            }
                        }

                        scanned_count += 1;
                    }

                    scanned_row += 1;
                }

                // Emit PageCompleted (if any items were processed) before scrolling
                // or exiting. Gives the caller a chance to drain per-page work
                // while the page is still in view.
                if page_had_items {
                    let action = callback(
                        &mut *self.ctrl,
                        GridEvent::PageCompleted { page_start_idx },
                    );
                    if matches!(action, ScanAction::Stop) {
                        break 'outer;
                    }
                }

                if stopped {
                    break 'outer;
                }
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

            let action = callback(&mut *self.ctrl, GridEvent::PageScrolled);
            if matches!(action, ScanAction::Stop) {
                break 'outer;
            }
        }
    }
}
