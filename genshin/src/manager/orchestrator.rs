use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use log::{info, warn};

use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::MappingManager;
use crate::scanner::common::models::GoodArtifact;

use super::lock_manager::LockManager;
use super::equip_manager::EquipManager;
use super::models::*;

/// Progress callback: (completed, total, current_instruction_id, phase_description).
pub type ProgressFn = dyn Fn(usize, usize, &str, &str);

/// Top-level artifact manager that orchestrates lock and equip operations.
///
/// 顶层圣遗物管理器，编排锁定和装备操作。
pub struct ArtifactManager {
    mappings: Arc<MappingManager>,
    ocr_backend: String,
    substat_ocr_backend: String,
    /// Delay (ms) between grid item clicks during backpack scan.
    pub delay_grid_item: u64,
    /// Delay (ms) after scrolling during backpack scan.
    pub delay_scroll: u64,
    /// If true, stop scanning as soon as all instructions are matched (faster,
    /// but produces an incomplete inventory snapshot).
    /// If false (default), always scan the full backpack for a complete snapshot.
    pub stop_on_all_matched: bool,
}

impl ArtifactManager {
    pub fn new(
        mappings: Arc<MappingManager>,
        ocr_backend: String,
        substat_ocr_backend: String,
    ) -> Self {
        Self {
            mappings,
            ocr_backend,
            substat_ocr_backend,
            delay_grid_item: 60,
            delay_scroll: 200,
            stop_on_all_matched: false,
        }
    }

    /// Execute all instructions in the request.
    ///
    /// Runs Phase 1 (lock changes via backpack scan) then Phase 2 (equip changes
    /// via character screens). The backpack scan results from Phase 1 are reused
    /// in Phase 2 for pre-flight validation.
    ///
    /// Returns `(ManageResult, Option<Vec<GoodArtifact>>)`. The second value is
    /// a complete artifact inventory snapshot (with lock states updated for
    /// successful toggles) if the backpack scan completed without interruption.
    /// `None` if no scan was performed or the scan was interrupted.
    ///
    /// 执行请求中的所有指令。先执行阶段一（通过背包扫描更改锁定），
    /// 再执行阶段二（通过角色界面更改装备）。
    /// 返回管理结果和完整圣遗物快照（如果背包扫描完成且未被中断）。
    pub fn execute(
        &self,
        ctrl: &mut GenshinGameController,
        request: ArtifactManageRequest,
        progress_fn: Option<&ProgressFn>,
    ) -> (ManageResult, Option<Vec<GoodArtifact>>) {
        let mut all_results: Vec<InstructionResult> = Vec::new();

        // Validate instructions before touching the game
        let mut valid_instructions: Vec<ArtifactInstruction> = Vec::new();
        for instr in &request.instructions {
            if let Some(err) = validate_instruction(instr) {
                warn!("[manager] 指令 {} 无效 / Instruction {} invalid: {}", instr.id, instr.id, err);
                all_results.push(InstructionResult {
                    id: instr.id.clone(),
                    status: InstructionStatus::InvalidInput,
                    detail: Some(err),
                });
            } else {
                valid_instructions.push(instr.clone());
            }
        }

        let invalid_count = all_results.len();
        if invalid_count > 0 {
            warn!(
                "[manager] {} 条指令无效，已过滤 / {} invalid instructions filtered out",
                invalid_count, invalid_count
            );
        }

        if valid_instructions.is_empty() {
            info!("[manager] 没有有效指令 / No valid instructions to execute");
            let summary = ManageSummary::from_results(&all_results);
            return (ManageResult { results: all_results, summary }, None);
        }

        let total = valid_instructions.len();

        // Replace request with validated instructions only
        let request = ArtifactManageRequest { instructions: valid_instructions };

        // Focus the game window before any UI interactions
        ctrl.focus_game_window();

        // Partition instructions
        let has_lock_changes: Vec<&ArtifactInstruction> = request.instructions.iter()
            .filter(|i| i.changes.lock.is_some())
            .collect();
        let has_equip_changes: Vec<&ArtifactInstruction> = request.instructions.iter()
            .filter(|i| i.changes.location.is_some())
            .collect();

        info!(
            "[manager] 执行 {} 条有效指令：{} 条锁定变更，{} 条装备变更 / \
             Executing {} valid instructions: {} lock changes, {} equip changes",
            request.instructions.len(),
            has_lock_changes.len(),
            has_equip_changes.len(),
            request.instructions.len(),
            has_lock_changes.len(),
            has_equip_changes.len(),
        );

        let report = |completed: usize, id: &str, phase: &str| {
            if let Some(f) = progress_fn {
                f(completed, total, id, phase);
            }
        };

        // Phase 1: Lock changes (iterate artifact backpack)
        let mut scanned_artifacts = Vec::new();
        let mut matched_ids: HashMap<String, usize> = HashMap::new();
        let mut scan_complete = false;
        let lock_instructions: Vec<ArtifactInstruction> = request.instructions.iter()
            .filter(|i| i.changes.lock.is_some())
            .cloned()
            .collect();

        if !lock_instructions.is_empty() && !yas::utils::was_aborted() {
            report(0, "", "阶段一：锁定变更 / Phase 1: Lock changes");

            let lock_mgr = LockManager::new(
                self.mappings.clone(),
                self.ocr_backend.clone(),
                self.substat_ocr_backend.clone(),
            );
            let (lock_results, artifacts, ids_map, complete) = lock_mgr.execute(
                ctrl,
                &lock_instructions,
                self.delay_grid_item,
                self.delay_scroll,
                self.stop_on_all_matched,
            );
            scanned_artifacts = artifacts;
            matched_ids = ids_map;
            scan_complete = complete;

            // Report progress for each lock result
            for r in &lock_results {
                all_results.push(r.clone());
                report(all_results.len(), &r.id, "阶段一：锁定变更 / Phase 1: Lock changes");
            }

            // Return to main UI between phases
            if !yas::utils::was_aborted() {
                ctrl.return_to_main_ui(4);
            }
        }

        // Phase 2: Equip changes (navigate character screens)
        let equip_instructions: Vec<ArtifactInstruction> = request.instructions.iter()
            .filter(|i| i.changes.location.is_some())
            .cloned()
            .collect();

        if !equip_instructions.is_empty() && !yas::utils::was_aborted() {
            report(all_results.len(), "", "阶段二：装备变更 / Phase 2: Equip changes");

            let equip_mgr = EquipManager::new(
                self.mappings.clone(),
                self.ocr_backend.clone(),
                self.substat_ocr_backend.clone(),
            );
            let equip_results = equip_mgr.execute(
                ctrl, &equip_instructions, &scanned_artifacts,
            );

            // Report progress for each equip result
            for r in &equip_results {
                all_results.push(r.clone());
                report(all_results.len(), &r.id, "阶段二：装备变更 / Phase 2: Equip changes");
            }
        }

        // Mark any remaining instructions as aborted
        let processed_ids: HashSet<String> = all_results.iter()
            .map(|r| r.id.clone())
            .collect();

        let was_aborted = yas::utils::was_aborted();
        for instr in &request.instructions {
            if !processed_ids.contains(&instr.id) {
                all_results.push(InstructionResult {
                    id: instr.id.clone(),
                    status: if was_aborted {
                        InstructionStatus::Aborted
                    } else {
                        InstructionStatus::Skipped
                    },
                    detail: Some(if was_aborted {
                        "用户中断 / User aborted".to_string()
                    } else {
                        "未处理 / Not processed".to_string()
                    }),
                });
            }
        }

        let summary = ManageSummary::from_results(&all_results);
        info!(
            "[manager] 完成：{} 成功, {} 已正确, {} 未找到, {} 错误, {} 中断 / \
             Done: {} success, {} already correct, {} not found, {} errors, {} aborted",
            summary.success, summary.already_correct, summary.not_found, summary.errors, summary.aborted,
            summary.success, summary.already_correct, summary.not_found, summary.errors, summary.aborted,
        );

        // Build artifact snapshot only if scan completed fully (all items visited,
        // no RMB abort, no early stop). Partial scans are not served.
        let artifact_snapshot = if scan_complete && !scanned_artifacts.is_empty() {
            Some(build_artifact_snapshot(
                &scanned_artifacts,
                &matched_ids,
                &all_results,
            ))
        } else {
            None
        };

        let result = ManageResult {
            results: all_results,
            summary,
        };

        (result, artifact_snapshot)
    }
}

/// Validate a single instruction. Returns Some(error_message) if invalid.
fn validate_instruction(instr: &ArtifactInstruction) -> Option<String> {
    if instr.changes.lock.is_none() && instr.changes.location.is_none() {
        return Some(
            "没有指定任何更改（lock 和 location 均为空） / \
             No changes specified (both lock and location are null)"
                .to_string(),
        );
    }
    if instr.target.set_key.trim().is_empty() {
        return Some("setKey 为空 / setKey is empty".to_string());
    }
    if instr.target.slot_key.trim().is_empty() {
        return Some("slotKey 为空 / slotKey is empty".to_string());
    }
    if instr.target.main_stat_key.trim().is_empty() {
        return Some("mainStatKey 为空 / mainStatKey is empty".to_string());
    }
    if instr.target.rarity < 1 || instr.target.rarity > 5 {
        return Some(format!(
            "稀有度无效: {} (应为 1-5) / Invalid rarity: {} (must be 1-5)",
            instr.target.rarity, instr.target.rarity
        ));
    }
    if instr.target.level < 0 || instr.target.level > 20 {
        return Some(format!(
            "等级无效: {} (应为 0-20) / Invalid level: {} (must be 0-20)",
            instr.target.level, instr.target.level
        ));
    }
    None
}

/// Build a flat artifact inventory snapshot with lock states updated for
/// successful toggles. Returns all scanned artifacts in grid order.
fn build_artifact_snapshot(
    scanned_artifacts: &[(usize, GoodArtifact)],
    matched_ids: &HashMap<String, usize>,
    results: &[InstructionResult],
) -> Vec<GoodArtifact> {
    // Build reverse map: scanned artifact index → status
    let mut index_to_status: HashMap<usize, &InstructionStatus> = HashMap::new();
    for (instr_id, &artifact_idx) in matched_ids {
        if let Some(r) = results.iter().find(|r| r.id == *instr_id) {
            index_to_status.insert(artifact_idx, &r.status);
        }
    }

    scanned_artifacts.iter().map(|(idx, artifact)| {
        match index_to_status.get(idx) {
            Some(InstructionStatus::Success) => {
                // Lock was toggled — reflect the new state
                let mut updated = artifact.clone();
                updated.lock = !updated.lock;
                updated
            }
            _ => artifact.clone(),
        }
    }).collect()
}
