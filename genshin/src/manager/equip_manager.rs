use std::collections::HashMap;
use std::sync::Arc;

use log::{info, warn};

use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::MappingManager;
use crate::scanner::common::models::GoodArtifact;
use crate::scanner::common::ocr_factory;

use super::models::*;
use super::ui_actions;

/// Phase 2: Navigate character screens and equip/unequip artifacts.
///
/// The game auto-swaps artifacts when equipping to a new character, so there
/// is no need to explicitly unequip from the previous owner.
///
/// 阶段二：导航角色界面，装备/卸下圣遗物。
/// 游戏在装备到新角色时会自动交换，无需手动卸下。
pub struct EquipManager {
    mappings: Arc<MappingManager>,
    ocr_backend: String,
    #[allow(dead_code)]
    substat_ocr_backend: String,
}

impl EquipManager {
    pub fn new(
        mappings: Arc<MappingManager>,
        ocr_backend: String,
        substat_ocr_backend: String,
    ) -> Self {
        Self { mappings, ocr_backend, substat_ocr_backend }
    }

    /// Execute equip/unequip instructions.
    ///
    /// `scanned_artifacts` is an optional list of artifacts found during Phase 1,
    /// used for pre-flight validation (e.g., detecting already-correct states).
    ///
    /// 执行装备/卸下指令。`scanned_artifacts` 是阶段一扫描到的圣遗物列表，
    /// 用于预检验证。
    pub fn execute(
        &self,
        ctrl: &mut GenshinGameController,
        instructions: &[ArtifactInstruction],
        scanned_artifacts: &[(usize, GoodArtifact)],
    ) -> Vec<InstructionResult> {
        let mut results: Vec<InstructionResult> = Vec::new();

        // Group instructions by target character
        let mut by_character: HashMap<String, Vec<&ArtifactInstruction>> = HashMap::new();
        for instr in instructions {
            if let Some(ref location) = instr.changes.location {
                by_character.entry(location.clone()).or_default().push(instr);
            }
        }

        // Create OCR model for artifact identification in selection list
        let ocr_model = match ocr_factory::create_ocr_model(&self.ocr_backend) {
            Ok(m) => m,
            Err(e) => {
                warn!("OCR模型创建失败 / OCR model creation failed: {}", e);
                return instructions.iter().map(|i| InstructionResult {
                    id: i.id.clone(),
                    status: InstructionStatus::OcrError,
                    detail: Some(format!("OCR模型创建失败 / OCR model creation failed: {}", e)),
                }).collect();
            }
        };

        let scaler = ctrl.scaler.clone();

        // Process unequip group first (location = "")
        if let Some(unequip_instrs) = by_character.remove("") {
            info!("[equip_manager] 处理 {} 条卸下指令 / Processing {} unequip instructions",
                unequip_instrs.len(), unequip_instrs.len());

            for instr in &unequip_instrs {
                if ctrl.check_rmb() {
                    results.push(InstructionResult {
                        id: instr.id.clone(),
                        status: InstructionStatus::Aborted,
                        detail: Some(format!("{}", ctrl.cancel_token().reason().unwrap())),
                    });
                    continue;
                }

                // For unequip, we need to know which character currently has it.
                // Check Phase 1 scan data for the artifact's current location.
                let current_location = scanned_artifacts.iter()
                    .find(|(_, a)| super::matching::match_score(a, &GoodArtifact::from(&instr.target)).is_some())
                    .map(|(_, a)| a.location.clone());

                match current_location {
                    Some(ref loc) if loc.is_empty() => {
                        // Already unequipped
                        results.push(InstructionResult {
                            id: instr.id.clone(),
                            status: InstructionStatus::AlreadyCorrect,
                            detail: Some("已经未装备 / Already unequipped".to_string()),
                        });
                    }
                    Some(ref char_key) => {
                        results.push(self.do_unequip(ctrl, instr, char_key));
                    }
                    None => {
                        // Artifact not found in Phase 1 scan
                        results.push(InstructionResult {
                            id: instr.id.clone(),
                            status: InstructionStatus::NotFound,
                            detail: Some(
                                "阶段一扫描中未找到该圣遗物，无法确定当前装备者 / \
                                 Artifact not found in Phase 1 scan, cannot determine current owner"
                                    .to_string(),
                            ),
                        });
                    }
                }
            }
        }

        // Process equip groups (one character at a time)
        for (char_key, char_instrs) in &by_character {
            if ctrl.check_rmb() {
                for instr in char_instrs {
                    results.push(InstructionResult {
                        id: instr.id.clone(),
                        status: InstructionStatus::Aborted,
                        detail: Some(format!("{}", ctrl.cancel_token().reason().unwrap())),
                    });
                }
                continue;
            }

            info!("[equip_manager] 处理角色 {} 的 {} 条装备指令 / Processing {} equip instructions for {}",
                char_key, char_instrs.len(), char_instrs.len(), char_key);

            // Pre-check: are any already equipped on this character?
            for instr in char_instrs {
                if let Some((_, ref scanned)) = scanned_artifacts.iter()
                    .find(|(_, a)| super::matching::match_score(a, &GoodArtifact::from(&instr.target)).is_some())
                {
                    if scanned.location == *char_key {
                        results.push(InstructionResult {
                            id: instr.id.clone(),
                            status: InstructionStatus::AlreadyCorrect,
                            detail: Some(format!(
                                "已装备在 {} 上 / Already equipped on {}",
                                char_key, char_key
                            )),
                        });
                        continue;
                    }
                }

                // Navigate to character and equip
                results.push(self.do_equip(ctrl, instr, char_key, ocr_model.as_ref(), &scaler));
            }
        }

        // Return to main UI after all equip operations
        if !results.is_empty() {
            ui_actions::leave_character_screen(ctrl).ok();
        }

        results
    }

    /// Navigate to a character and equip an artifact.
    fn do_equip(
        &self,
        ctrl: &mut GenshinGameController,
        instr: &ArtifactInstruction,
        char_key: &str,
        ocr: &dyn yas::ocr::ImageToText<image::RgbImage>,
        scaler: &crate::scanner::common::coord_scaler::CoordScaler,
    ) -> InstructionResult {
        // Step 1: Navigate to character screen
        if let Err(e) = ui_actions::open_character_screen(ctrl, char_key, &self.mappings) {
            return InstructionResult {
                id: instr.id.clone(),
                status: InstructionStatus::UiError,
                detail: Some(format!(
                    "无法导航到角色 {} / Cannot navigate to character {}: {}",
                    char_key, char_key, e
                )),
            };
        }

        // Step 2: Click the artifact slot
        if let Err(e) = ui_actions::click_equipment_slot(ctrl, &instr.target.slot_key) {
            return InstructionResult {
                id: instr.id.clone(),
                status: InstructionStatus::UiError,
                detail: Some(format!(
                    "无法点击装备栏 {} / Cannot click slot {}: {}",
                    instr.target.slot_key, instr.target.slot_key, e
                )),
            };
        }

        // Step 3: Find and click the artifact in selection list
        match ui_actions::find_and_click_artifact_in_selection(
            ctrl, &instr.target, ocr, scaler, &self.mappings,
        ) {
            Ok(true) => {
                // Artifact found and clicked — game auto-equips
                InstructionResult {
                    id: instr.id.clone(),
                    status: InstructionStatus::Success,
                    detail: None,
                }
            }
            Ok(false) => {
                InstructionResult {
                    id: instr.id.clone(),
                    status: InstructionStatus::NotFound,
                    detail: Some(
                        "在选择列表中未找到目标圣遗物 / \
                         Target artifact not found in selection list"
                            .to_string(),
                    ),
                }
            }
            Err(e) => {
                InstructionResult {
                    id: instr.id.clone(),
                    status: InstructionStatus::UiError,
                    detail: Some(format!(
                        "选择列表扫描失败 / Selection list scan failed: {}", e
                    )),
                }
            }
        }
    }

    /// Navigate to a character and unequip an artifact from the given slot.
    fn do_unequip(
        &self,
        ctrl: &mut GenshinGameController,
        instr: &ArtifactInstruction,
        current_owner: &str,
    ) -> InstructionResult {
        // Step 1: Navigate to the current owner's character screen
        if let Err(e) = ui_actions::open_character_screen(ctrl, current_owner, &self.mappings) {
            return InstructionResult {
                id: instr.id.clone(),
                status: InstructionStatus::UiError,
                detail: Some(format!(
                    "无法导航到角色 {} / Cannot navigate to character {}: {}",
                    current_owner, current_owner, e
                )),
            };
        }

        // Step 2: Click the artifact slot
        if let Err(e) = ui_actions::click_equipment_slot(ctrl, &instr.target.slot_key) {
            return InstructionResult {
                id: instr.id.clone(),
                status: InstructionStatus::UiError,
                detail: Some(format!(
                    "无法点击装备栏 {} / Cannot click slot {}: {}",
                    instr.target.slot_key, instr.target.slot_key, e
                )),
            };
        }

        // Step 3: Click unequip
        if let Err(e) = ui_actions::click_unequip_button(ctrl) {
            return InstructionResult {
                id: instr.id.clone(),
                status: InstructionStatus::UiError,
                detail: Some(format!("卸下失败 / Unequip failed: {}", e)),
            };
        }

        InstructionResult {
            id: instr.id.clone(),
            status: InstructionStatus::Success,
            detail: None,
        }
    }
}
