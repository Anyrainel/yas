use std::collections::HashMap;
use std::sync::Arc;

use image::RgbImage;
use yas::{log_debug, log_info, log_warn};

use yas::ocr::ImageToText;
use crate::scanner::common::constants::*;
use crate::scanner::common::coord_scaler::CoordScaler;
use crate::scanner::common::debug_dump::DumpCtx;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::MappingManager;
use crate::scanner::common::models::GoodArtifact;
use crate::scanner::common::ocr_pool::SharedOcrPools;

use super::models::*;
use super::ui_actions;
use super::ui_actions::{d_transition, d_action, d_cell};

/// A single equip/unequip target with a result ID for tracking.
pub struct EquipTarget {
    pub result_id: String,
    pub artifact: GoodArtifact,
    pub target_location: String,
}

/// Executes equip/unequip operations by navigating character screens.
pub struct EquipManager {
    mappings: Arc<MappingManager>,
    pools: Arc<SharedOcrPools>,
    dump_images: bool,
}

impl EquipManager {
    pub fn new(
        mappings: Arc<MappingManager>,
        pools: Arc<SharedOcrPools>,
        dump_images: bool,
    ) -> Self {
        Self { mappings, pools, dump_images }
    }

    /// Execute a list of equip/unequip targets.
    ///
    /// Strategy: scan through characters in roster order (pressing "next" button).
    /// For each character that has equip targets, open the artifact selection and
    /// process all 5 slots smartly using set-based filtering.
    pub fn execute(
        &self,
        ctrl: &mut GenshinGameController,
        targets: &[EquipTarget],
        progress_fn: Option<&crate::scanner::common::progress::ProgressFn<'_>>,
    ) -> Vec<InstructionResult> {
        let ocr = self.pools.v4().get();

        let scaler = ctrl.scaler.clone();
        let mut results: HashMap<String, InstructionResult> = HashMap::new();
        let total = targets.len();
        let report = |completed: usize, id: &str| {
            if let Some(pf) = progress_fn {
                pf(completed, total, id, "装备变更 / Equip changes");
            }
        };
        report(0, "");

        // Separate unequip and equip targets — always attempt equip (don't trust location field)
        let unequip_targets: Vec<&EquipTarget> = targets.iter()
            .filter(|t| t.target_location.is_empty())
            .collect();

        let equip_targets: Vec<&EquipTarget> = targets.iter()
            .filter(|t| !t.target_location.is_empty())
            .collect();

        // Group equip targets by character
        let mut char_groups: HashMap<&str, Vec<&EquipTarget>> = HashMap::new();
        for target in &equip_targets {
            char_groups.entry(target.target_location.as_str())
                .or_default()
                .push(target);
        }

        // Process unequip targets first (each needs the artifact's current owner)
        for target in &unequip_targets {
            if ctrl.check_rmb() {
                results.insert(target.result_id.clone(), InstructionResult {
                    id: target.result_id.clone(),
                    status: InstructionStatus::Aborted,
                });
                report(results.len(), &target.result_id);
                continue;
            }
            let result = self.do_unequip(ctrl, target, &target.artifact.location, &ocr);
            results.insert(target.result_id.clone(), result);
            report(results.len(), &target.result_id);
        }

        // Process equip targets by scanning character roster in order
        if !char_groups.is_empty() {
            self.process_equip_by_roster_scan(
                ctrl, &char_groups, &ocr, &scaler, &mut results, progress_fn, total,
            );
        }

        // Leave character screen after all operations
        let _ = ui_actions::leave_character_screen(ctrl);

        // Return results in target order
        targets.iter().map(|t| {
            results.remove(&t.result_id).unwrap_or(InstructionResult {
                id: t.result_id.clone(),
                status: InstructionStatus::Skipped,
            })
        }).collect()
    }

    /// Scan through the character roster in order, processing equip targets
    /// for each character as encountered.
    fn process_equip_by_roster_scan(
        &self,
        ctrl: &mut GenshinGameController,
        char_groups: &HashMap<&str, Vec<&EquipTarget>>,
        ocr: &dyn yas::ocr::ImageToText<image::RgbImage>,
        scaler: &CoordScaler,
        results: &mut HashMap<String, InstructionResult>,
        progress_fn: Option<&crate::scanner::common::progress::ProgressFn<'_>>,
        total: usize,
    ) {
        let report = |completed: usize, id: &str| {
            if let Some(pf) = progress_fn {
                pf(completed, total, id, "装备变更 / Equip changes");
            }
        };
        // Open character screen (same logic as ensure_character_screen used by tests)
        ctrl.return_to_main_ui(8);
        yas::utils::sleep(d_action() * 5 / 8);

        if let Err(e) = ui_actions::ensure_character_screen(ctrl, ocr, &self.mappings) {
            log_warn!("[equip_roster] 无法打开角色界面: {}", "[equip_roster] cannot open character screen: {}", e);
            for targets in char_groups.values() {
                for target in targets {
                    results.insert(target.result_id.clone(), InstructionResult {
                        id: target.result_id.clone(),
                        status: InstructionStatus::UiError,
                    });
                }
            }
            return;
        }

        let max_chars = 150;
        let mut first_name: Option<String> = None;
        let mut remaining_chars = char_groups.len();

        for i in 0..max_chars {
            if ctrl.check_rmb() || remaining_chars == 0 {
                break;
            }

            // OCR character name
            let name_text = ctrl.ocr_region(ocr, CHAR_NAME_RECT).unwrap_or_default();
            let name_trimmed = name_text.trim().to_string();
            log_debug!("[equip_roster] #{}: OCR识别='{}'", "[equip_roster] #{}: OCR='{}'", i, name_trimmed);

            if self.dump_images {
                if let Ok(image) = ctrl.capture_game() {
                    let ctx = DumpCtx::new("debug_images", "manager_equip", i, "");
                    ctx.dump_full(&image);
                    ctx.dump_region("char_name", &image, CHAR_NAME_RECT, scaler);
                }
            }

            // Check for full cycle
            if i > 0 {
                if let Some(ref first) = first_name {
                    let cur_name = clean_char_name(&name_trimmed);
                    let first_char_name = clean_char_name(first);
                    if !cur_name.is_empty() && cur_name == first_char_name {
                        log_debug!("[equip_roster] 已遍历全部角色", "[equip_roster] all characters visited (cycled)");
                        break;
                    }
                }
            }
            if first_name.is_none() && !name_trimmed.is_empty() {
                first_name = Some(name_trimmed.clone());
            }

            // Try to match this character against any remaining char_groups
            let clean_name = clean_char_name(&name_trimmed);
            let matched_key = self.match_character_name(&name_trimmed, &clean_name, char_groups, results);

            if let Some(char_key) = matched_key {
                log_debug!("[equip_roster] 在位置{}找到{}", "[equip_roster] found {} at position {}", i, char_key);

                if let Some(targets) = char_groups.get(char_key.as_str()) {
                    let slot_targets: Vec<&EquipTarget> = targets.iter()
                        .filter(|t| !results.contains_key(&t.result_id))
                        .copied()
                        .collect();

                    if !slot_targets.is_empty() {
                        self.equip_character_slots(ctrl, &slot_targets, ocr, scaler, results);
                        remaining_chars -= 1;
                        report(results.len(), &char_key);
                    }
                }

                // After processing, we're back on the character screen.
                // We need to navigate back to the character roster first.
                // Press Escape to leave any sub-screen and get back to character detail.
                // The character detail should still show the same character.
            }

            // Click next character
            ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
            yas::utils::sleep(250);
        }

        // Mark any remaining targets as not found
        for targets in char_groups.values() {
            for target in targets {
                if !results.contains_key(&target.result_id) {
                    results.insert(target.result_id.clone(), InstructionResult {
                        id: target.result_id.clone(),
                        status: InstructionStatus::NotFound,
                    });
                }
            }
        }
        report(results.len(), "");
    }

    /// Try to match the OCR'd character name against any of the target character keys.
    fn match_character_name(
        &self,
        name_trimmed: &str,
        clean_name: &str,
        char_groups: &HashMap<&str, Vec<&EquipTarget>>,
        results: &HashMap<String, InstructionResult>,
    ) -> Option<String> {
        for &char_key in char_groups.keys() {
            // Skip characters that are already fully processed
            let has_remaining = char_groups[char_key].iter()
                .any(|t| !results.contains_key(&t.result_id));
            if !has_remaining {
                continue;
            }

            // Direct GOOD key match
            if name_trimmed.contains(char_key) {
                return Some(char_key.to_string());
            }

            // Chinese name match
            let cn_names: Vec<String> = self.mappings
                .character_name_map
                .iter()
                .filter(|(_, v)| v.as_str() == char_key)
                .map(|(k, _)| k.clone())
                .collect();

            for cn in &cn_names {
                if name_trimmed.contains(cn.as_str()) {
                    return Some(char_key.to_string());
                }
                if fuzzy_char_match(clean_name, cn) {
                    return Some(char_key.to_string());
                }
            }

            // Reverse mapping lookup
            for try_name in &[name_trimmed, clean_name] {
                if let Some(matched_key) = self.mappings.character_name_map.get(*try_name) {
                    if matched_key.as_str() == char_key {
                        return Some(char_key.to_string());
                    }
                }
            }
        }
        None
    }

    /// Process all equip targets for one character, using smart set-based slot ordering.
    ///
    /// Strategy:
    /// 1. Determine the set composition (4pc, 2+2pc, etc.)
    /// 2. Order: flex slot first (own set filter), then main slots (shared filter)
    /// 3. The selection view stays open after equip — just switch slot tabs
    fn equip_character_slots(
        &self,
        ctrl: &mut GenshinGameController,
        targets: &[&EquipTarget],
        ocr: &dyn yas::ocr::ImageToText<image::RgbImage>,
        _scaler: &CoordScaler,
        results: &mut HashMap<String, InstructionResult>,
    ) {
        // Click 圣遗物 menu to show artifact circles
        ctrl.focus_game_window();
        yas::utils::sleep(d_cell() * 2);
        ctrl.click_at(160.0, 293.0); // CHAR_ARTIFACT_MENU
        yas::utils::sleep(d_transition() * 4 / 5);

        // Click "替换" to open selection view
        ctrl.click_at(1720.0, 1010.0); // CHAR_REPLACE_BUTTON
        yas::utils::sleep(d_transition() * 4 / 3);

        // Analyze set composition: flex slots (single-count sets) first, main slots last.
        // The main set filter is applied last so the game remembers it.
        let (flex_indices, main_sets) = analyze_set_composition(targets);

        log_debug!("[equip_slots] flex={:?}, main_sets={:?}, {} 个目标", "[equip_slots] flex={:?}, main_sets={:?}, analyzing {} targets",
            flex_indices, main_sets, targets.len());

        // Build ordered slot list: all flex first, then all non-flex
        let mut ordered: Vec<(usize, &EquipTarget, bool)> = Vec::new(); // (idx, target, is_flex)
        for &fi in &flex_indices {
            ordered.push((fi, targets[fi], true));
        }
        for (i, t) in targets.iter().enumerate() {
            if !flex_indices.contains(&i) {
                ordered.push((i, t, false));
            }
        }

        // Track which set filter is currently active so we don't re-apply unnecessarily.
        let mut active_filter: Option<Vec<String>> = None;

        for &(_idx, target, is_flex) in &ordered {
            if ctrl.check_rmb() {
                results.insert(target.result_id.clone(), InstructionResult {
                    id: target.result_id.clone(),
                    status: InstructionStatus::Aborted,
                });
                continue;
            }
            if results.contains_key(&target.result_id) {
                continue;
            }

            // Flex slots: each gets its own single-set filter.
            // Main slots: share the main multi-set filter (applied once, stays active).
            let needed_filter: Vec<String> = if is_flex {
                vec![target.artifact.set_key.clone()]
            } else {
                main_sets.clone()
            };

            // Apply set filter if it differs from what's active
            if active_filter.as_ref() != Some(&needed_filter) {
                if needed_filter.len() == 1 {
                    let ok = ui_actions::apply_set_filter(
                        ctrl, &needed_filter[0], &self.mappings, ocr,
                    ).unwrap_or(false);
                    if ok {
                        active_filter = Some(needed_filter.clone());
                    }
                } else {
                    let set_refs: Vec<&str> = needed_filter.iter().map(|s| s.as_str()).collect();
                    let count = ui_actions::apply_multi_set_filter(
                        ctrl, &set_refs, &self.mappings, ocr,
                    ).unwrap_or(0);
                    if count > 0 {
                        active_filter = Some(needed_filter.clone());
                    }
                }
            }

            // Click slot tab
            if let Err(e) = ui_actions::click_slot_tab(ctrl, &target.artifact.slot_key) {
                log_warn!("[equip_slot] 点击栏位标签失败: {}", "[equip_slot] click slot tab failed: {}", e);
                results.insert(target.result_id.clone(), InstructionResult {
                    id: target.result_id.clone(),
                    status: InstructionStatus::UiError,
                });
                continue;
            }

            // Check if the currently equipped artifact already matches (live OCR check)
            match ui_actions::check_current_artifact_matches(ctrl, &target.artifact, ocr, &self.mappings) {
                Ok(true) => {
                    log_info!("[equip_slot] {} 已正确装备", "[equip_slot] {} already correct", target.result_id);
                    results.insert(target.result_id.clone(), InstructionResult {
                        id: target.result_id.clone(),
                        status: InstructionStatus::AlreadyCorrect,
                    });
                    continue;
                }
                Ok(false) => { /* need to find the artifact */ }
                Err(e) => {
                    log_debug!("[equip_slot] 检查当前圣遗物失败: {}", "[equip_slot] check current failed: {}", e);
                }
            }

            // Search the grid for the target artifact
            match ui_actions::find_artifact_in_grid_with_dump(ctrl, &target.artifact, ocr, &self.mappings, true, self.dump_images) {
                Ok(true) => {
                    log_info!("[equip_slot] 装备成功: {}", "[equip_slot] equip success: {}", target.result_id);
                    results.insert(target.result_id.clone(), InstructionResult {
                        id: target.result_id.clone(),
                        status: InstructionStatus::Success,
                    });
                }
                Ok(false) => {
                    log_warn!("[equip_slot] 未找到: {}", "[equip_slot] not found: {}", target.result_id);
                    results.insert(target.result_id.clone(), InstructionResult {
                        id: target.result_id.clone(),
                        status: InstructionStatus::NotFound,
                    });
                }
                Err(e) => {
                    log_warn!("[equip_slot] 装备失败: {}", "[equip_slot] equip error: {}", e);
                    results.insert(target.result_id.clone(), InstructionResult {
                        id: target.result_id.clone(),
                        status: InstructionStatus::UiError,
                    });
                }
            }
        }

        // Return to character detail screen
        ctrl.key_press(enigo::Key::Escape);
        yas::utils::sleep(d_action());
    }

    fn do_unequip(
        &self,
        ctrl: &mut GenshinGameController,
        target: &EquipTarget,
        current_owner: &str,
        ocr: &dyn ImageToText<RgbImage>,
    ) -> InstructionResult {
        if let Err(e) = ui_actions::open_character_screen(ctrl, current_owner, &self.mappings, ocr) {
            log_warn!("[equip_manager] 打开角色界面失败: {}", "[equip_manager] open character screen failed: {}", e);
            return InstructionResult {
                id: target.result_id.clone(),
                status: InstructionStatus::UiError,
            };
        }

        if let Err(e) = ui_actions::click_equipment_slot(ctrl, &target.artifact.slot_key) {
            log_warn!("[equip_manager] 点击装备栏位失败: {}", "[equip_manager] click equipment slot failed: {}", e);
            return InstructionResult {
                id: target.result_id.clone(),
                status: InstructionStatus::UiError,
            };
        }

        if let Err(e) = ui_actions::click_unequip_button(ctrl) {
            log_warn!("[equip_manager] 卸下操作失败: {}", "[equip_manager] unequip failed: {}", e);
            return InstructionResult {
                id: target.result_id.clone(),
                status: InstructionStatus::UiError,
            };
        }

        log_info!("[equip_manager] 卸下成功: {} (from {})", "[equip_manager] unequip success: {} (from {})", target.result_id, current_owner);
        InstructionResult {
            id: target.result_id.clone(),
            status: InstructionStatus::Success,
        }
    }
}

/// Analyze the set composition of equip targets for a character.
///
/// Returns:
/// - `flex_indices`: Indices of targets whose set appears only once (processed first,
///   each with its own temporary filter)
/// - `main_sets`: Sets with count > 1 (applied as a persistent multi-set filter last,
///   so the game remembers it)
///
/// Examples:
/// - (5)       → flex=[], main=[A]
/// - (4+1)     → flex=[idx of B], main=[A]
/// - (3+2)     → flex=[], main=[A,B]
/// - (2+2+1)   → flex=[idx of C], main=[A,B]
/// - (3+1+1)   → flex=[idx of B, idx of C], main=[A]
/// - (2+1+1+1) → flex=[idx of B,C,D], main=[A]
/// - (1+1+1+1+1) → flex=[all indices], main=[] (all processed one by one)
fn analyze_set_composition(targets: &[&EquipTarget]) -> (Vec<usize>, Vec<String>) {
    let mut set_counts: HashMap<&str, usize> = HashMap::new();
    for t in targets {
        *set_counts.entry(t.artifact.set_key.as_str()).or_insert(0) += 1;
    }

    // Main = sets with count > 1, flex = sets with count == 1
    let main_sets: Vec<String> = set_counts.iter()
        .filter(|(_, &v)| v > 1)
        .map(|(&k, _)| k.to_string())
        .collect();

    let flex_set_keys: Vec<&str> = set_counts.iter()
        .filter(|(_, &v)| v == 1)
        .map(|(&k, _)| k)
        .collect();

    // Collect indices of all flex targets
    let flex_indices: Vec<usize> = targets.iter().enumerate()
        .filter(|(_, t)| flex_set_keys.contains(&t.artifact.set_key.as_str()))
        .map(|(i, _)| i)
        .collect();

    (flex_indices, main_sets)
}

// Re-export helper functions from ui_actions module scope
fn clean_char_name(text: &str) -> String {
    let name = extract_char_name(text);
    let chars: Vec<char> = name.chars().collect();
    let mut end = chars.len();
    while end > 0 {
        let c = chars[end - 1];
        if c >= '\u{4e00}' && c <= '\u{9fff}' {
            break;
        }
        if c >= '\u{3040}' && c <= '\u{30ff}' {
            break;
        }
        end -= 1;
    }
    chars[..end].iter().collect()
}

fn extract_char_name(text: &str) -> String {
    let trimmed = text.trim();
    for sep in &['/', '／', '丨', '|'] {
        if let Some(pos) = trimmed.rfind(*sep) {
            let after = &trimmed[pos + sep.len_utf8()..].trim();
            if !after.is_empty() {
                return after.to_string();
            }
        }
    }
    if let Some(pos) = trimmed.rfind("元素") {
        let after = &trimmed[pos + "元素".len()..].trim();
        if !after.is_empty() {
            return after.to_string();
        }
    }
    trimmed.to_string()
}

fn fuzzy_char_match(ocr_name: &str, expected: &str) -> bool {
    let ocr_chars: Vec<char> = ocr_name.chars().collect();
    let exp_chars: Vec<char> = expected.chars().collect();
    if ocr_chars.len() != exp_chars.len() || ocr_chars.len() < 2 {
        return false;
    }
    let diffs = ocr_chars.iter().zip(exp_chars.iter())
        .filter(|(a, b)| a != b)
        .count();
    diffs <= 1
}
