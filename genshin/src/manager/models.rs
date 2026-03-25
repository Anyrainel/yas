use serde::{Deserialize, Serialize};

use crate::scanner::common::models::GoodSubStat;

/// A batch of artifact change instructions.
/// 圣遗物管理指令批次。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactManageRequest {
    pub instructions: Vec<ArtifactInstruction>,
}

/// A single artifact management instruction.
/// 单条圣遗物管理指令。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactInstruction {
    /// Unique identifier for this instruction (client-assigned, for tracking).
    /// 指令唯一标识（由客户端分配，用于跟踪）。
    pub id: String,

    /// Fields used to identify the target artifact in-game.
    /// 用于在游戏中识别目标圣遗物的字段。
    pub target: ArtifactTarget,

    /// What changes to apply. At least one field must be Some.
    /// 要应用的更改。至少一个字段必须为 Some。
    pub changes: ArtifactChanges,
}

/// Identity of an artifact — enough fields to uniquely match one in-game.
/// All fields are required for reliable matching.
///
/// 圣遗物身份标识——用于在游戏中唯一匹配一个圣遗物。
/// 所有字段都是必需的。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactTarget {
    #[serde(rename = "setKey")]
    pub set_key: String,
    #[serde(rename = "slotKey")]
    pub slot_key: String,
    pub rarity: i32,
    pub level: i32,
    #[serde(rename = "mainStatKey")]
    pub main_stat_key: String,
    /// Active substats (order-independent for matching).
    /// 副属性（匹配时不考虑顺序）。
    pub substats: Vec<GoodSubStat>,
}

/// Changes to apply to the matched artifact.
/// 要应用到匹配圣遗物的更改。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactChanges {
    /// If Some, set lock to this state.
    /// 如果为 Some，将锁定状态设置为此值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<bool>,

    /// If Some, change equipment. Empty string means unequip.
    /// Non-empty string is the GOOD character key to equip to.
    /// The game auto-swaps when equipping to a new character.
    ///
    /// 如果为 Some，更改装备。空字符串表示卸下。
    /// 非空字符串是要装备到的 GOOD 角色键名。
    /// 游戏在装备到新角色时会自动交换。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

// ---------------------------------------------------------------------------
// Output models
// ---------------------------------------------------------------------------

/// Full result of a manage operation.
/// 管理操作的完整结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManageResult {
    pub results: Vec<InstructionResult>,
    pub summary: ManageSummary,
}

/// Per-instruction outcome.
/// 每条指令的执行结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstructionResult {
    /// Matches ArtifactInstruction.id
    pub id: String,
    pub status: InstructionStatus,
    /// Human-readable detail (bilingual).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InstructionStatus {
    /// Change applied successfully.
    Success,
    /// Artifact was found but already in the desired state.
    AlreadyCorrect,
    /// No matching artifact found during inventory scan.
    NotFound,
    /// OCR failed while trying to identify the artifact.
    OcrError,
    /// In-game UI interaction failed.
    UiError,
    /// User aborted via RMB.
    Aborted,
    /// Skipped because a prerequisite step failed.
    Skipped,
    /// Input data is invalid (missing changes, empty keys, etc.).
    InvalidInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManageSummary {
    pub total: usize,
    pub success: usize,
    pub already_correct: usize,
    pub not_found: usize,
    pub errors: usize,
    pub aborted: usize,
}

impl ManageSummary {
    pub fn from_results(results: &[InstructionResult]) -> Self {
        let mut summary = ManageSummary {
            total: results.len(),
            success: 0,
            already_correct: 0,
            not_found: 0,
            errors: 0,
            aborted: 0,
        };
        for r in results {
            match r.status {
                InstructionStatus::Success => summary.success += 1,
                InstructionStatus::AlreadyCorrect => summary.already_correct += 1,
                InstructionStatus::NotFound => summary.not_found += 1,
                InstructionStatus::OcrError | InstructionStatus::UiError => summary.errors += 1,
                InstructionStatus::Aborted => summary.aborted += 1,
                InstructionStatus::Skipped | InstructionStatus::InvalidInput => summary.errors += 1,
            }
        }
        summary
    }
}

// ---------------------------------------------------------------------------
// Async job state
// ---------------------------------------------------------------------------

/// Phase of an async manage job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Idle,
    Running,
    Completed,
}

/// Progress of a running job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProgress {
    pub completed: usize,
    pub total: usize,
    /// ID of the instruction currently being processed.
    #[serde(rename = "currentId")]
    pub current_id: String,
    /// Human-readable phase description.
    pub phase: String,
}

/// Shared state for an async manage job, polled via GET /status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobState {
    pub state: JobPhase,
    #[serde(rename = "jobId", skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<JobProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ManageResult>,
}

impl JobState {
    pub fn idle() -> Self {
        Self { state: JobPhase::Idle, job_id: None, progress: None, result: None }
    }

    pub fn running(job_id: String, total: usize) -> Self {
        Self {
            state: JobPhase::Running,
            job_id: Some(job_id),
            progress: Some(JobProgress {
                completed: 0,
                total,
                current_id: String::new(),
                phase: String::new(),
            }),
            result: None,
        }
    }

    pub fn completed(job_id: String, result: ManageResult) -> Self {
        Self {
            state: JobPhase::Completed,
            job_id: Some(job_id),
            progress: None,
            result: Some(result),
        }
    }
}
