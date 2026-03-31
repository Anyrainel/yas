use std::collections::HashMap;
use std::sync::Arc;

use crossbeam_channel;

use log::{debug, info, warn};

use crate::scanner::artifact::GoodArtifactScanner;
use crate::scanner::common::backpack_scanner;
use crate::scanner::common::constants::*;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::MappingManager;
use crate::scanner::common::models::GoodArtifact;
use crate::scanner::common::ocr_factory;
use crate::scanner::common::ocr_pool::OcrPool;
use crate::scanner::common::pixel_utils;

use super::matching;
use super::models::*;
use super::ui_actions;

/// Phase 1: Single-pass artifact backpack scan with per-page lock toggling.
///
/// Reuses the same backpack opening, grid coordinates, two-phase capture,
/// and `identify_artifact` OCR as the artifact scanner. Lock toggles are
/// applied per-page before scrolling, so there is no second pass.
///
/// 阶段一：单次遍历圣遗物背包，每页扫描后直接切换锁定。
/// 复用圣遗物扫描器的背包打开、网格坐标、两阶段截图和 OCR。
pub struct LockManager {
    mappings: Arc<MappingManager>,
    ocr_backend: String,
    substat_ocr_backend: String,
}

/// An artifact identified on the current page that needs a lock toggle.
struct PageToggle {
    instr_id: String,
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
        ocr_backend: String,
        substat_ocr_backend: String,
    ) -> Self {
        Self { mappings, ocr_backend, substat_ocr_backend }
    }

    /// Execute lock change instructions by scanning the artifact backpack.
    ///
    /// Returns results for all provided instructions plus the list of scanned
    /// artifacts (for Phase 2 reuse).
    ///
    /// Uses the same opening sequence and grid navigation as the artifact
    /// scanner, with lock toggles applied per-page before scrolling.
    ///
    /// 执行锁定变更指令。返回：
    /// - 所有指令的执行结果
    /// - 扫描到的圣遗物列表 (index, artifact)
    /// - 指令ID→圣遗物索引的映射（用于分类）
    /// - 扫描是否完整（遍历了所有圣遗物，未被中断或提前终止）
    pub fn execute(
        &self,
        ctrl: &mut GenshinGameController,
        instructions: &[ArtifactInstruction],
        delay_grid_item: u64,
        delay_scroll: u64,
        stop_on_all_matched: bool,
    ) -> (Vec<InstructionResult>, Vec<(usize, GoodArtifact)>, HashMap<String, usize>, bool) {
        let mut results: HashMap<String, InstructionResult> = HashMap::new();
        let mut scanned_artifacts: Vec<(usize, GoodArtifact)> = Vec::new();

        // Create OCR pools (same sizes as artifact scanner: 2 main + 5 substat)
        let ocr_backend = self.ocr_backend.clone();
        let ocr_pool = match OcrPool::new(
            move || ocr_factory::create_ocr_model(&ocr_backend),
            2,
        ) {
            Ok(p) => Arc::new(p),
            Err(e) => {
                warn!("OCR模型创建失败 / OCR model creation failed: {}", e);
                return (
                    instructions.iter().map(|i| InstructionResult {
                        id: i.id.clone(),
                        status: InstructionStatus::OcrError,
                        detail: Some(format!("OCR模型创建失败 / OCR model creation failed: {}", e)),
                    }).collect(),
                    scanned_artifacts,
                    HashMap::new(),
                    false,
                );
            }
        };
        let substat_backend = self.substat_ocr_backend.clone();
        let substat_pool = match OcrPool::new(
            move || ocr_factory::create_ocr_model(&substat_backend),
            5,
        ) {
            Ok(p) => Arc::new(p),
            Err(e) => {
                warn!("副属性OCR模型创建失败 / Substat OCR model creation failed: {}", e);
                return (
                    instructions.iter().map(|i| InstructionResult {
                        id: i.id.clone(),
                        status: InstructionStatus::OcrError,
                        detail: Some(format!("OCR模型创建失败 / OCR model creation failed: {}", e)),
                    }).collect(),
                    scanned_artifacts,
                    HashMap::new(),
                    false,
                );
            }
        };
        // Single model for count OCR (used once during backpack open)
        let count_ocr = match ocr_factory::create_ocr_model(&self.ocr_backend) {
            Ok(m) => m,
            Err(e) => {
                warn!("OCR模型创建失败 / OCR model creation failed: {}", e);
                return (
                    instructions.iter().map(|i| InstructionResult {
                        id: i.id.clone(),
                        status: InstructionStatus::OcrError,
                        detail: Some(format!("OCR模型创建失败 / OCR model creation failed: {}", e)),
                    }).collect(),
                    scanned_artifacts,
                    HashMap::new(),
                    false,
                );
            }
        };

        // Build targets for matching (only instructions with lock changes)
        let targets: Vec<(usize, &ArtifactTarget)> = instructions.iter()
            .enumerate()
            .filter(|(_, i)| i.changes.lock.is_some())
            .map(|(idx, i)| (idx, &i.target))
            .collect();

        if targets.is_empty() {
            return (Vec::new(), scanned_artifacts, HashMap::new(), false);
        }

        // Track which instructions have been matched
        let mut matched: HashMap<usize, usize> = HashMap::new();
        // instruction_id → scanned artifact index (for categorization by caller)
        let mut matched_ids: HashMap<String, usize> = HashMap::new();

        // --- Open backpack to artifact tab (same as artifact scanner) ---
        let total = match backpack_scanner::open_backpack_to_tab(
            ctrl, "artifact", 1200, 400, count_ocr.as_ref(),
        ) {
            Ok((count, _max)) => {
                info!("[lock_manager] 共 {} 个圣遗物 / {} artifacts total", count, count);
                count as usize
            }
            Err(e) => {
                warn!("无法读取圣遗物数量 / Cannot read artifact count: {}", e);
                return (
                    instructions.iter().map(|i| InstructionResult {
                        id: i.id.clone(),
                        status: InstructionStatus::OcrError,
                        detail: Some(format!("无法读取数量 / Cannot read count: {}", e)),
                    }).collect(),
                    scanned_artifacts,
                    HashMap::new(),
                    false,
                );
            }
        };

        if total == 0 {
            info!("[lock_manager] 背包中没有圣遗物 / No artifacts in backpack");
            return (
                instructions.iter().filter(|i| i.changes.lock.is_some()).map(|i| InstructionResult {
                    id: i.id.clone(),
                    status: InstructionStatus::NotFound,
                    detail: Some("背包中没有圣遗物 / No artifacts in backpack".to_string()),
                }).collect(),
                scanned_artifacts,
                HashMap::new(),
                true, // empty backpack is a "complete" scan
            );
        }

        let scaler = ctrl.scaler.clone();
        let total_rows = (total + GRID_COLS - 1) / GRID_COLS;
        let last_row_cols = if total % GRID_COLS == 0 { GRID_COLS } else { total % GRID_COLS };

        // Timing: same two-phase capture as artifact scanner
        let delay_after_panel = delay_grid_item; // same as scanner
        let first_delay = delay_after_panel / 2;
        let retry_delay = delay_after_panel;

        // Click first item to ensure focus
        ctrl.click_at(GRID_FIRST_X, GRID_FIRST_Y);
        yas::utils::sleep(300);

        let mut scanned_count: usize = 0;
        let mut scanned_row: usize = 0;
        let mut pages_scrolled: u32 = 0;

        let scaler_arc = Arc::new(scaler.clone());

        // --- Per-page scan + lock toggle loop ---
        'outer: while scanned_count < total {
            let visible_rows = GRID_ROWS.min(total_rows - scanned_row);

            // Result channel for pipelined OCR
            let (result_tx, result_rx) = crossbeam_channel::unbounded::<(usize, usize, usize, Option<GoodArtifact>)>();
            let mut dispatched: usize = 0;

            // Step 1: Click through items, capture, dispatch OCR to rayon immediately
            for row in 0..visible_rows {
                let row_cols = if scanned_row + row == total_rows - 1 {
                    last_row_cols
                } else {
                    GRID_COLS
                };

                for col in 0..row_cols {
                    if ctrl.check_rmb() || scanned_count >= total {
                        break 'outer;
                    }

                    let x = GRID_FIRST_X + col as f64 * GRID_OFFSET_X;
                    let y = GRID_FIRST_Y + row as f64 * GRID_OFFSET_Y;
                    ctrl.click_at(x, y);

                    // Wait for panel to load (same as scan_grid)
                    let _ = ctrl.wait_until_panel_loaded(PANEL_POOL_RECT, 400);

                    // Two-phase capture with retry for lock/astral animation
                    if first_delay > 0 {
                        yas::utils::sleep(first_delay as u32);
                    }

                    let mut image = match ctrl.capture_game() {
                        Ok(img) => img,
                        Err(e) => {
                            warn!("[lock_manager] 截图失败 #{}: {} / [lock_manager] capture failed #{}: {}", scanned_count, e, scanned_count, e);
                            scanned_count += 1;
                            continue;
                        }
                    };

                    // Retry if icon brightness is ambiguous (mid-animation)
                    if retry_delay > 0 && pixel_utils::is_artifact_icon_ambiguous(&image, &scaler) {
                        yas::utils::sleep(retry_delay as u32);
                        image = match ctrl.capture_game() {
                            Ok(img) => img,
                            Err(e) => {
                                warn!("[lock_manager] 重试截图失败 #{}: {} / [lock_manager] retry capture failed #{}: {}", scanned_count, e, scanned_count, e);
                                scanned_count += 1;
                                continue;
                            }
                        };
                    }

                    // Dispatch OCR to rayon immediately (runs in parallel with next capture)
                    let tx = result_tx.clone();
                    let pool = ocr_pool.clone();
                    let sub_pool = substat_pool.clone();
                    let sc = scaler_arc.clone();
                    let mp = self.mappings.clone();
                    let idx = scanned_count;
                    let r = row;
                    let c = col;
                    rayon::spawn(move || {
                        let ocr = pool.get();
                        let sub_ocr = sub_pool.get();
                        let artifact = match GoodArtifactScanner::identify_artifact(
                            &ocr as &dyn yas::ocr::ImageToText<image::RgbImage>,
                            &sub_ocr as &dyn yas::ocr::ImageToText<image::RgbImage>,
                            &image, &sc, &mp,
                        ) {
                            Ok(a) => a,
                            Err(e) => {
                                warn!("[lock_manager] OCR失败 #{}: {} / OCR failed", idx, e);
                                None
                            }
                        };
                        let _ = tx.send((idx, r, c, artifact));
                    });
                    dispatched += 1;

                    scanned_count += 1;
                }

                scanned_row += 1;
            }

            // Step 2: Collect all OCR results for this page
            drop(result_tx); // close sender so recv terminates
            let mut page_results: Vec<(usize, usize, usize, Option<GoodArtifact>)> =
                Vec::with_capacity(dispatched);
            for result in result_rx {
                page_results.push(result);
            }
            // Sort by index to maintain grid order
            page_results.sort_by_key(|(idx, _, _, _)| *idx);

            // Step 3: Match against instructions, collect per-page toggles
            let mut page_toggles: Vec<PageToggle> = Vec::new();

            for (idx, row, col, artifact_opt) in &page_results {
                if let Some(ref artifact) = artifact_opt {
                    scanned_artifacts.push((*idx, artifact.clone()));

                    // Match against unmatched instructions
                    let unmatched_converted: Vec<(usize, GoodArtifact)> = targets.iter()
                        .filter(|(i, _)| !matched.contains_key(i))
                        .map(|&(i, t)| (i, GoodArtifact::from(t)))
                        .collect();
                    let unmatched: Vec<(usize, &GoodArtifact)> = unmatched_converted.iter()
                        .map(|(i, a)| (*i, a))
                        .collect();

                    if let Some((instr_idx, _score)) = matching::find_best_match(artifact, &unmatched) {
                        matched.insert(instr_idx, *idx);
                        let instr = &instructions[instr_idx];
                        matched_ids.insert(instr.id.clone(), *idx);
                        let desired_lock = instr.changes.lock.unwrap();

                        if artifact.lock == desired_lock {
                            results.insert(instr.id.clone(), InstructionResult {
                                id: instr.id.clone(),
                                status: InstructionStatus::AlreadyCorrect,
                                detail: Some(format!(
                                    "锁定状态已正确 / Lock already {}",
                                    if desired_lock { "locked" } else { "unlocked" }
                                )),
                            });
                        } else {
                            let y_shift = if artifact.elixir_crafted { 40.0 } else { 0.0 };
                            page_toggles.push(PageToggle {
                                instr_id: instr.id.clone(),
                                row: *row,
                                col: *col,
                                desired_lock,
                                y_shift,
                            });
                        }
                    }
                }
            }

            // Step 4: Apply lock toggles for this page (before scrolling)
            for toggle in &page_toggles {
                if ctrl.check_rmb() {
                    results.insert(toggle.instr_id.clone(), InstructionResult {
                        id: toggle.instr_id.clone(),
                        status: InstructionStatus::Aborted,
                        detail: Some(format!("{}", ctrl.cancel_token().reason().unwrap())),
                    });
                    continue;
                }

                // Click back to the grid position
                let x = GRID_FIRST_X + toggle.col as f64 * GRID_OFFSET_X;
                let y = GRID_FIRST_Y + toggle.row as f64 * GRID_OFFSET_Y;
                ctrl.click_at(x, y);
                let _ = ctrl.wait_until_panel_loaded(PANEL_POOL_RECT, 400);
                yas::utils::sleep(100);

                // Toggle lock
                if let Err(e) = ui_actions::click_lock_button(ctrl, toggle.y_shift) {
                    results.insert(toggle.instr_id.clone(), InstructionResult {
                        id: toggle.instr_id.clone(),
                        status: InstructionStatus::UiError,
                        detail: Some(format!("锁定切换失败 / Lock toggle failed: {}", e)),
                    });
                    continue;
                }

                // Verify lock state changed
                yas::utils::sleep(200);
                let image = match ctrl.capture_game() {
                    Ok(img) => img,
                    Err(e) => {
                        warn!("[lock_manager] 截图失败 / Capture failed: {}", e);
                        results.insert(toggle.instr_id.clone(), InstructionResult {
                            id: toggle.instr_id.clone(),
                            status: InstructionStatus::UiError,
                            detail: Some(format!("截图失败 / Capture failed: {}", e)),
                        });
                        continue;
                    }
                };
                let new_lock = pixel_utils::detect_artifact_lock(&image, &scaler, toggle.y_shift);

                if new_lock == toggle.desired_lock {
                    info!(
                        "[lock_manager] 锁定切换成功 ({},{}) / Lock toggle success",
                        toggle.row, toggle.col
                    );
                    results.insert(toggle.instr_id.clone(), InstructionResult {
                        id: toggle.instr_id.clone(),
                        status: InstructionStatus::Success,
                        detail: None,
                    });
                } else {
                    warn!(
                        "[lock_manager] 锁定验证失败 ({},{}): 期望={} 实际={} / Lock verify failed",
                        toggle.row, toggle.col, toggle.desired_lock, new_lock
                    );
                    results.insert(toggle.instr_id.clone(), InstructionResult {
                        id: toggle.instr_id.clone(),
                        status: InstructionStatus::UiError,
                        detail: Some(format!(
                            "锁定切换验证失败（期望={}，实际={}）/ \
                             Lock toggle verification failed (expected={}, actual={})",
                            toggle.desired_lock, new_lock, toggle.desired_lock, new_lock
                        )),
                    });
                }
            }

            // Early stop if all instructions matched (only when configured)
            if stop_on_all_matched && matched.len() == targets.len() {
                info!("[lock_manager] 所有指令已匹配，提前停止 / All instructions matched, stopping early");
                break;
            }

            // Step 5: Scroll to next page
            let remain = total.saturating_sub(scanned_count);
            if remain == 0 {
                break;
            }
            let remain_rows = (remain + GRID_COLS - 1) / GRID_COLS;
            let scroll_rows = remain_rows.min(GRID_ROWS);

            // Move mouse to grid center for scroll
            let center_x = GRID_FIRST_X + 3.0 * GRID_OFFSET_X;
            let center_y = GRID_FIRST_Y + 2.0 * GRID_OFFSET_Y;
            ctrl.move_to(center_x, center_y);
            yas::utils::sleep(30);

            let mut ticks = (SCROLL_TICKS_PER_PAGE as f64 / GRID_ROWS as f64 * scroll_rows as f64).round() as i32;
            if scroll_rows == GRID_ROWS {
                pages_scrolled += 1;
                if SCROLL_CORRECTION_INTERVAL > 0
                    && pages_scrolled % SCROLL_CORRECTION_INTERVAL as u32 == 0
                {
                    ticks -= 1;
                    debug!("[lock_manager] 滚动修正在第{}页（-1 tick） / [lock_manager] scroll correction at page {} (-1 tick)", pages_scrolled, pages_scrolled);
                }
            }

            for i in 0..ticks {
                if ctrl.check_rmb() {
                    break 'outer;
                }
                ctrl.mouse_scroll(1);
                if (i + 1) % 5 == 0 {
                    yas::utils::sleep(10);
                }
            }
            yas::utils::sleep(delay_scroll as u32);
        }

        // Scan is complete only if we visited every item without interruption
        let scan_complete = scanned_count >= total && !ctrl.is_cancelled();

        // Mark unmatched instructions
        let was_cancelled = ctrl.is_cancelled();
        for instr in instructions {
            if instr.changes.lock.is_some() && !results.contains_key(&instr.id) {
                results.insert(instr.id.clone(), InstructionResult {
                    id: instr.id.clone(),
                    status: if was_cancelled {
                        InstructionStatus::Aborted
                    } else {
                        InstructionStatus::NotFound
                    },
                    detail: Some(if was_cancelled {
                        format!("{}", ctrl.cancel_token().reason().unwrap_or(yas::cancel::StopReason::UserAbort))
                    } else {
                        "背包中未找到匹配圣遗物 / Matching artifact not found in inventory".to_string()
                    }),
                });
            }
        }

        // Collect results in instruction order
        let ordered_results: Vec<InstructionResult> = instructions.iter()
            .filter_map(|i| results.remove(&i.id))
            .collect();

        (ordered_results, scanned_artifacts, matched_ids, scan_complete)
    }
}
