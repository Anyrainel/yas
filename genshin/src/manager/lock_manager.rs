use std::collections::HashMap;
use std::sync::Arc;

use crossbeam_channel;

use yas::{log_debug, log_info, log_warn};

use crate::scanner::artifact::GoodArtifactScanner;
use crate::scanner::common::backpack_scanner::{
    self, BackpackScanConfig, BackpackScanner, GridEvent, ScanAction,
};
use crate::scanner::common::constants::*;
use super::ui_actions::{d_action, d_cell};
use crate::scanner::common::debug_dump::DumpCtx;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::grid_icon_detector::{GridIconResult, GridMode};
use crate::scanner::common::grid_voter::{PagedGridVoter, ReadyItem};
use crate::scanner::common::mappings::MappingManager;
use crate::scanner::common::models::GoodArtifact;
use crate::scanner::common::ocr_pool::SharedOcrPools;
use crate::scanner::common::pixel_utils;

use super::matching;
use super::models::*;
use super::orchestrator::LockTarget;
use super::ui_actions;

/// Single-pass artifact backpack scan with per-page lock toggling.
///
/// Accepts `LockTarget` slices from the orchestrator. Each target specifies
/// an artifact to match and the desired lock state. Rarity early-stop:
/// when a scanned artifact has rarity < 4, the current page is finished
/// but no further items are dispatched.
///
/// 单次遍历圣遗物背包，每页扫描后直接切换锁定。
/// 当检测到稀有度 < 4 时，完成当前页后停止扫描。
pub struct LockManager {
    mappings: Arc<MappingManager>,
    pools: Arc<SharedOcrPools>,
}

/// An artifact identified on the current page that needs a lock toggle.
struct PageToggle {
    result_id: String,
    /// Row within the current visible page (0-based).
    row: usize,
    /// Column (0-based).
    col: usize,
    /// Desired lock state.
    desired_lock: bool,
    /// Y-shift for elixir artifacts.
    y_shift: f64,
}

/// Panel pool rect for wait_until_panel_loaded (same as backpack_scanner).
const PANEL_POOL_RECT: (f64, f64, f64, f64) = (1400.0, 300.0, 300.0, 200.0);

impl LockManager {
    pub fn new(
        mappings: Arc<MappingManager>,
        pools: Arc<SharedOcrPools>,
    ) -> Self {
        Self { mappings, pools }
    }

    /// Execute lock change targets by scanning the artifact backpack.
    ///
    /// Returns:
    /// - Results for processed targets
    /// - Scanned artifacts as (index, artifact)
    /// - Map from target vec index -> scanned artifact index (for snapshot building)
    /// - Whether the scan completed fully (all items visited, no interruption)
    ///
    /// 执行锁定变更目标。返回：
    /// - 已处理目标的结果
    /// - 扫描到的圣遗物列表 (index, artifact)
    /// - 目标向量索引→圣遗物索引的映射
    /// - 扫描是否完整
    pub fn execute(
        &self,
        ctrl: &mut GenshinGameController,
        targets: &[LockTarget],
        capture_delay: u64,
        delay_scroll: u64,
        stop_on_all_matched: bool,
        max_target_level: i32,
        dump_images: bool,
    ) -> (Vec<InstructionResult>, Vec<(usize, GoodArtifact)>, HashMap<usize, usize>, bool) {
        let mut results: HashMap<String, InstructionResult> = HashMap::new();
        let mut scanned_artifacts: Vec<(usize, GoodArtifact)> = Vec::new();

        let make_error_results = |targets: &[LockTarget], status: InstructionStatus| -> Vec<InstructionResult> {
            targets.iter().map(|t| InstructionResult {
                id: t.result_id.clone(),
                status: status.clone(),
            }).collect()
        };

        if targets.is_empty() {
            return (Vec::new(), scanned_artifacts, HashMap::new(), false);
        }

        // Use shared OCR pools (v5 for level, v4 for everything else).
        let ocr_pool = self.pools.v5().clone();
        let substat_pool = self.pools.v4().clone();
        // Borrow a model from the v5 pool for reading item count
        let count_ocr_guard = ocr_pool.get();

        // Track which targets have been matched (target vec index -> scanned artifact index)
        let mut matched: HashMap<usize, usize> = HashMap::new();

        // --- Open backpack to artifact tab (same as artifact scanner) ---
        let total = match backpack_scanner::open_backpack_to_tab(
            ctrl, "artifact", 1200, 400, &count_ocr_guard,
        ) {
            Ok((count, _max)) => {
                log_debug!("[lock_manager] 共 {} 个圣遗物", "[lock_manager] {} artifacts total", count);
                count as usize
            }
            Err(e) => {
                log_warn!("无法读取圣遗物数量: {}", "Cannot read artifact count: {}", e);
                return (
                    make_error_results(targets, InstructionStatus::OcrError),
                    scanned_artifacts,
                    HashMap::new(),
                    false,
                );
            }
        };

        if total == 0 {
            log_info!("[lock_manager] 背包中没有圣遗物，无法执行锁定操作", "[lock_manager] No artifacts in backpack, cannot perform lock operations");
            return (
                targets.iter().map(|t| InstructionResult {
                    id: t.result_id.clone(),
                    status: InstructionStatus::NotFound,
                }).collect(),
                scanned_artifacts,
                HashMap::new(),
                true, // empty backpack is a "complete" scan
            );
        }

        // Return count OCR model to pool before scan loop
        drop(count_ocr_guard);

        let scaler = ctrl.scaler.clone();
        let scaler_arc = Arc::new(scaler.clone());

        // Pre-focus the first grid cell (unchanged behavior from previous impl).
        ctrl.click_at(GRID_FIRST_X, GRID_FIRST_Y);
        yas::utils::sleep(d_action() * 3 / 8);

        // --- Per-page callback state ---
        // A fresh OCR result channel is created before each page; PageCompleted
        // drains it and re-creates one for the next page.
        type OcrResult = (usize, usize, usize, Option<GoodArtifact>);
        let (init_tx, init_rx) = crossbeam_channel::unbounded::<OcrResult>();
        let mut result_tx: crossbeam_channel::Sender<OcrResult> = init_tx;
        let mut result_rx: crossbeam_channel::Receiver<OcrResult> = init_rx;
        let mut dispatched: usize = 0;

        // Per-page 3-pass voter (payload carries the (row, col) needed to
        // re-click the grid cell for lock toggling).
        let mut voter: PagedGridVoter<(usize, usize)> = PagedGridVoter::new(total, GridMode::Artifact);

        // Scan-wide flags.
        let mut rarity_stopped = false;
        let mut stop_requested = false;
        let mut toggle_counter: usize = 0;
        // Once the last-cell probe shows a level within range (or OCR succeeds
        // with level <= max_target_level), we stop probing and scan every item.
        // OCR failure (-1) does NOT count as "in range" — keep probing.
        let mut level_in_range = false;

        // Build a scan_grid config that enables the per-page level probe
        // when a max target level has been computed (fast mode).
        let scan_config = BackpackScanConfig {
            delay_scroll,
            delay_before_capture: capture_delay,
            probe_last_cell_per_page: max_target_level >= 0,
        };

        // Clones for closure capture.
        let ocr_pool_cb = ocr_pool.clone();
        let substat_pool_cb = substat_pool.clone();
        let mappings_cb = self.mappings.clone();
        let scaler_cb = scaler.clone();

        let mut bp = BackpackScanner::new(ctrl);
        bp.scan_grid(total, &scan_config, 0, |ctrl_cb, event| {
            match event {
                // ---------------- Page probe: level-based skip ----------------
                GridEvent::PageStarted { page_start_idx, last_cell_image } => {
                    if max_target_level < 0 || level_in_range {
                        return ScanAction::Continue;
                    }
                    let ocr_guard = ocr_pool_cb.get();
                    let level = GoodArtifactScanner::scan_level_only(
                        &ocr_guard as &dyn yas::ocr::ImageToText<image::RgbImage>,
                        &last_cell_image,
                        &scaler_cb,
                    );
                    if level > max_target_level {
                        log_debug!(
                            "[lock_manager] 页面跳过：末尾等级={} > 最高目标等级={} (page_start={})",
                            "[lock_manager] Page skip: last level={} > max target={} (page_start={})",
                            level, max_target_level, page_start_idx
                        );
                        ScanAction::SkipPage
                    } else if level >= 0 {
                        // Level is in range — stop probing on future pages.
                        level_in_range = true;
                        log_debug!(
                            "[lock_manager] 末尾等级={} 在范围内，后续页面不再探测 (page_start={})",
                            "[lock_manager] Last level={} in range, disabling probe for future pages (page_start={})",
                            level, page_start_idx
                        );
                        ScanAction::Continue
                    } else {
                        // OCR failed (-1) — scan this page but keep probing.
                        log_debug!(
                            "[lock_manager] 末尾等级OCR失败，扫描此页但继续探测 (page_start={})",
                            "[lock_manager] Last level OCR failed, scanning page but keeping probe (page_start={})",
                            page_start_idx
                        );
                        ScanAction::Continue
                    }
                }

                // ---------------- Per-item voting + OCR dispatch ----------------
                GridEvent::Item { idx, row, col, image } => {
                    if dump_images {
                        let ctx = DumpCtx::new("debug_images", "manager_lock", idx, "");
                        ctx.dump_full(&image);
                    }

                    // Dispatch helper: spawns a rayon OCR task for one ready item.
                    let dispatch = |ready: Vec<ReadyItem<(usize, usize)>>,
                                    dispatched: &mut usize| {
                        for item in ready {
                            let d_idx = item.idx;
                            let (d_row, d_col) = item.payload;
                            let d_img = item.image;
                            let gi: Option<GridIconResult> = item.metadata;
                            let tx = result_tx.clone();
                            let pool = ocr_pool_cb.clone();
                            let sub_pool = substat_pool_cb.clone();
                            let sc = scaler_arc.clone();
                            let mp = mappings_cb.clone();
                            rayon::spawn(move || {
                                let ocr = pool.get();
                                let sub_ocr = sub_pool.get();
                                let artifact = match GoodArtifactScanner::identify_artifact(
                                    &ocr as &dyn yas::ocr::ImageToText<image::RgbImage>,
                                    &sub_ocr as &dyn yas::ocr::ImageToText<image::RgbImage>,
                                    &d_img, &sc, &mp, gi,
                                ) {
                                    Ok(a) => a,
                                    Err(e) => { log_warn!("[lock_manager] OCR失败 #{}: {}", "[lock_manager] OCR failed #{}: {}", d_idx, e); None }
                                };
                                let _ = tx.send((d_idx, d_row, d_col, artifact));
                            });
                            *dispatched += 1;
                        }
                    };

                    // Rarity early-stop: low-rarity lv0 artifact → stop after
                    // current page finishes (PageCompleted will drain and
                    // process toggles for whatever was dispatched so far).
                    if pixel_utils::artifact_below_min_rarity(&image, &scaler_cb, 4)
                        && {
                            let guard = ocr_pool_cb.get();
                            GoodArtifactScanner::scan_level_only(&guard, &image, &scaler_cb) <= 0
                        }
                    {
                        log_debug!(
                            "[lock_manager] 检测到低稀有度lv0圣遗物，当前页后停止",
                            "[lock_manager] Low rarity lv0 artifact detected, stopping after current page"
                        );
                        rarity_stopped = true;
                        stop_requested = true;
                        let ready = voter.early_stop_flush(&image, idx, &scaler_cb);
                        dispatch(ready, &mut dispatched);
                        return ScanAction::Stop;
                    }

                    let ready = voter.record(idx, image, (row, col), &scaler_cb);
                    dispatch(ready, &mut dispatched);

                    ScanAction::Continue
                }

                // ---------------- Drain + match + toggle locks ----------------
                GridEvent::PageCompleted { .. } => {
                    // Flush any still-deferred items via voter final_flush
                    // (tie-breaks on the last deferred image if needed).
                    let leftover = voter.final_flush(&scaler_cb);
                    for item in leftover {
                        let d_idx = item.idx;
                        let (d_row, d_col) = item.payload;
                        let d_img = item.image;
                        let gi: Option<GridIconResult> = item.metadata;
                        let tx = result_tx.clone();
                        let pool = ocr_pool_cb.clone();
                        let sub_pool = substat_pool_cb.clone();
                        let sc = scaler_arc.clone();
                        let mp = mappings_cb.clone();
                        rayon::spawn(move || {
                            let ocr = pool.get();
                            let sub_ocr = sub_pool.get();
                            let artifact = match GoodArtifactScanner::identify_artifact(
                                &ocr as &dyn yas::ocr::ImageToText<image::RgbImage>,
                                &sub_ocr as &dyn yas::ocr::ImageToText<image::RgbImage>,
                                &d_img, &sc, &mp, gi,
                            ) {
                                Ok(a) => a,
                                Err(e) => { log_warn!("[lock_manager] OCR失败 #{}: {}", "[lock_manager] OCR failed #{}: {}", d_idx, e); None }
                            };
                            let _ = tx.send((d_idx, d_row, d_col, artifact));
                        });
                        dispatched += 1;
                    }

                    // Drain OCR results for this page.
                    let (fresh_tx, fresh_rx) = crossbeam_channel::unbounded::<OcrResult>();
                    let old_tx = std::mem::replace(&mut result_tx, fresh_tx);
                    drop(old_tx);
                    let old_rx = std::mem::replace(&mut result_rx, fresh_rx);
                    let mut page_results: Vec<OcrResult> = Vec::with_capacity(dispatched);
                    for r in old_rx {
                        page_results.push(r);
                    }
                    page_results.sort_by_key(|(idx, _, _, _)| *idx);
                    dispatched = 0;

                    // Match against unmatched targets; collect toggles.
                    let mut page_toggles: Vec<PageToggle> = Vec::new();
                    for (idx, row, col, artifact_opt) in &page_results {
                        if let Some(ref artifact) = artifact_opt {
                            scanned_artifacts.push((*idx, artifact.clone()));

                            let unmatched: Vec<(usize, &GoodArtifact)> = targets.iter()
                                .enumerate()
                                .filter(|(i, _)| !matched.contains_key(i))
                                .map(|(i, t)| (i, &t.artifact))
                                .collect();

                            if let Some((target_idx, _score)) =
                                matching::find_best_match(artifact, &unmatched)
                            {
                                matched.insert(target_idx, *idx);
                                let target = &targets[target_idx];
                                if artifact.lock == target.desired_lock {
                                    results.insert(target.result_id.clone(), InstructionResult {
                                        id: target.result_id.clone(),
                                        status: InstructionStatus::AlreadyCorrect,
                                    });
                                } else {
                                    let y_shift = if artifact.elixir_crafted { 40.0 } else { 0.0 };
                                    page_toggles.push(PageToggle {
                                        result_id: target.result_id.clone(),
                                        row: *row,
                                        col: *col,
                                        desired_lock: target.desired_lock,
                                        y_shift,
                                    });
                                }
                            }
                        }
                    }

                    // Apply lock toggles (unchanged behavior: delays, click
                    // sequence, pixel verification).
                    for toggle in &page_toggles {
                        if ctrl_cb.check_rmb() {
                            results.insert(toggle.result_id.clone(), InstructionResult {
                                id: toggle.result_id.clone(),
                                status: InstructionStatus::Aborted,
                            });
                            continue;
                        }

                        let x = GRID_FIRST_X + toggle.col as f64 * GRID_OFFSET_X;
                        let y = GRID_FIRST_Y + toggle.row as f64 * GRID_OFFSET_Y;
                        ctrl_cb.click_at(x, y);
                        let _ = ctrl_cb.wait_until_panel_loaded(PANEL_POOL_RECT, 400);
                        yas::utils::sleep(d_cell());

                        if let Err(e) = ui_actions::click_lock_button(ctrl_cb, toggle.y_shift) {
                            log_warn!("[lock_manager] 锁定切换失败: {}", "[lock_manager] Lock toggle failed: {}", e);
                            results.insert(toggle.result_id.clone(), InstructionResult {
                                id: toggle.result_id.clone(),
                                status: InstructionStatus::UiError,
                            });
                            continue;
                        }

                        yas::utils::sleep(d_cell() * 2);
                        let image = match ctrl_cb.capture_game() {
                            Ok(img) => img,
                            Err(e) => {
                                log_warn!("[lock_manager] 截图失败: {}", "[lock_manager] Capture failed: {}", e);
                                results.insert(toggle.result_id.clone(), InstructionResult {
                                    id: toggle.result_id.clone(),
                                    status: InstructionStatus::UiError,
                                });
                                continue;
                            }
                        };
                        let verified = pixel_utils::verify_artifact_lock_toggled(
                            &image, &scaler_cb, toggle.y_shift, toggle.desired_lock,
                        );
                        let new_lock = if verified {
                            toggle.desired_lock
                        } else {
                            pixel_utils::detect_artifact_lock(&image, &scaler_cb, toggle.y_shift)
                        };

                        if dump_images {
                            let ctx = DumpCtx::new(
                                "debug_images", "manager_lock_verify", toggle_counter, "",
                            );
                            ctx.dump_full(&image);
                            ctx.dump_pixel("lock_px", &image, ARTIFACT_LOCK_POS1, 5, &scaler_cb);
                        }
                        toggle_counter += 1;

                        if new_lock == toggle.desired_lock {
                            if toggle.desired_lock {
                                log_debug!("[lock_manager] 锁定成功 ({},{})", "[lock_manager] Lock success ({},{})", toggle.row, toggle.col);
                            } else {
                                log_debug!("[lock_manager] 解锁成功 ({},{})", "[lock_manager] Unlock success ({},{})", toggle.row, toggle.col);
                            }
                            results.insert(toggle.result_id.clone(), InstructionResult {
                                id: toggle.result_id.clone(),
                                status: InstructionStatus::Success,
                            });
                        } else {
                            log_warn!(
                                "[lock_manager] 锁定验证失败 ({},{}): 期望={} 实际={}",
                                "[lock_manager] Lock verify failed ({},{}): expected={} actual={}",
                                toggle.row, toggle.col, toggle.desired_lock, new_lock
                            );
                            results.insert(toggle.result_id.clone(), InstructionResult {
                                id: toggle.result_id.clone(),
                                status: InstructionStatus::UiError,
                            });
                        }
                    }

                    // Early stop if all targets matched (fast mode only).
                    if stop_on_all_matched && matched.len() == targets.len() {
                        log_info!("[lock_manager] 所有目标已匹配，提前停止", "[lock_manager] All targets matched, stopping early");
                        stop_requested = true;
                    }

                    if stop_requested {
                        ScanAction::Stop
                    } else {
                        ScanAction::Continue
                    }
                }

                // ---------------- Reset voter state for next page ----------------
                GridEvent::PageScrolled => {
                    voter.reset_page();
                    ScanAction::Continue
                }
            }
        });

        // Compute scan completeness. Rarity early-stop counts as a complete
        // scan (we've visited every ≥4★ artifact). When stop_on_all_matched is
        // enabled, pages may be skipped (level-based page-skip) and the scan
        // stops early — the scanned data is always partial, so never produce
        // a snapshot.
        let scanned_all = scanned_artifacts.last().map(|(idx, _)| *idx + 1 >= total).unwrap_or(false);
        let scan_complete = !stop_on_all_matched
            && (scanned_all || rarity_stopped)
            && !ctrl.is_cancelled();

        // Mark unmatched targets.
        let was_cancelled = ctrl.is_cancelled();
        for target in targets {
            if !results.contains_key(&target.result_id) {
                results.insert(target.result_id.clone(), InstructionResult {
                    id: target.result_id.clone(),
                    status: if was_cancelled {
                        InstructionStatus::Aborted
                    } else {
                        InstructionStatus::NotFound
                    },
                });
            }
        }

        let ordered_results: Vec<InstructionResult> = targets.iter()
            .filter_map(|t| results.remove(&t.result_id))
            .collect();

        (ordered_results, scanned_artifacts, matched, scan_complete)
    }
}
