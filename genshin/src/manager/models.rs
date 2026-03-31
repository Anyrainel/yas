use serde::{Deserialize, Serialize};

use crate::scanner::common::models::{GoodArtifact, GoodSubStat};

/// Lock/unlock request: two lists of artifacts in GOOD v3 format.
///
/// Each artifact represents the client's view of its **current state**.
/// Which list it appears in determines the desired lock action:
/// - `lock`: these artifacts should be locked after execution
/// - `unlock`: these artifacts should be unlocked after execution
///
/// The artifact's own `lock` field is ignored for determining intention —
/// only list membership matters. This allows stale data to still express
/// the correct intention.
///
/// 锁定/解锁请求：两个 GOOD v3 格式的圣遗物列表。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockManageRequest {
    /// Artifacts that should be locked.
    #[serde(default)]
    pub lock: Vec<GoodArtifact>,
    /// Artifacts that should be unlocked.
    #[serde(default)]
    pub unlock: Vec<GoodArtifact>,
}

// ---------------------------------------------------------------------------
// Legacy types — kept temporarily for other modules still referencing them.
// Will be removed as subsequent tasks migrate callers to LockManageRequest.
// ---------------------------------------------------------------------------

/// DEPRECATED: Use `LockManageRequest` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactManageRequest {
    pub instructions: Vec<ArtifactInstruction>,
}

/// DEPRECATED: Use `LockManageRequest` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactInstruction {
    pub id: String,
    pub target: ArtifactTarget,
    pub changes: ArtifactChanges,
}

/// DEPRECATED: Use `GoodArtifact` for matching instead.
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
    pub substats: Vec<GoodSubStat>,
}

impl From<&ArtifactTarget> for GoodArtifact {
    fn from(t: &ArtifactTarget) -> Self {
        GoodArtifact {
            set_key: t.set_key.clone(),
            slot_key: t.slot_key.clone(),
            rarity: t.rarity,
            level: t.level,
            main_stat_key: t.main_stat_key.clone(),
            substats: t.substats.clone(),
            location: String::new(),
            lock: false,
            astral_mark: false,
            elixir_crafted: false,
            unactivated_substats: Vec::new(),
            total_rolls: None,
        }
    }
}

/// DEPRECATED: Use list membership in `LockManageRequest` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactChanges {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<bool>,
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

    /// Lightweight JSON for polling — excludes the full result payload.
    ///
    /// Returns state + jobId + progress (when running) or summary (when completed).
    /// The full result is only available via `GET /result`.
    ///
    /// 轻量级 JSON 用于轮询——不包含完整结果。完整结果通过 GET /result 获取。
    pub fn status_json(&self) -> String {
        // jobId is always a UUID v4 (hex + hyphens), safe to embed directly.
        match self.state {
            JobPhase::Idle => r#"{"state":"idle"}"#.to_string(),
            JobPhase::Running => {
                let job_id = self.job_id.as_deref().unwrap_or("");
                if let Some(ref p) = self.progress {
                    format!(
                        r#"{{"state":"running","jobId":"{}","progress":{{"completed":{},"total":{}}}}}"#,
                        job_id, p.completed, p.total
                    )
                } else {
                    format!(r#"{{"state":"running","jobId":"{}"}}"#, job_id)
                }
            }
            JobPhase::Completed => {
                let job_id = self.job_id.as_deref().unwrap_or("");
                if let Some(ref r) = self.result {
                    let s = &r.summary;
                    format!(
                        r#"{{"state":"completed","jobId":"{}","summary":{{"total":{},"success":{},"already_correct":{},"not_found":{},"errors":{},"aborted":{}}}}}"#,
                        job_id, s.total, s.success, s.already_correct,
                        s.not_found, s.errors, s.aborted
                    )
                } else {
                    format!(r#"{{"state":"completed","jobId":"{}"}}"#, job_id)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_manage_request_deser() {
        let json = r#"{
            "lock": [{
                "setKey": "GladiatorsFinale",
                "slotKey": "flower",
                "rarity": 5,
                "level": 20,
                "mainStatKey": "hp",
                "substats": [{"key": "critRate_", "value": 3.9}],
                "location": "",
                "lock": false
            }],
            "unlock": [{
                "setKey": "WanderersTroupe",
                "slotKey": "plume",
                "rarity": 5,
                "level": 16,
                "mainStatKey": "atk",
                "substats": [{"key": "hp", "value": 508.0}],
                "location": "Furina",
                "lock": true
            }]
        }"#;
        let req: LockManageRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.lock.len(), 1);
        assert_eq!(req.unlock.len(), 1);
        assert_eq!(req.lock[0].set_key, "GladiatorsFinale");
        assert_eq!(req.unlock[0].set_key, "WanderersTroupe");
    }

    #[test]
    fn test_lock_manage_request_empty_lists() {
        let json = r#"{"lock": [], "unlock": []}"#;
        let req: LockManageRequest = serde_json::from_str(json).unwrap();
        assert!(req.lock.is_empty());
        assert!(req.unlock.is_empty());
    }

    #[test]
    fn test_lock_manage_request_one_list_only() {
        let json = r#"{
            "lock": [{
                "setKey": "GladiatorsFinale", "slotKey": "flower",
                "rarity": 5, "level": 20, "mainStatKey": "hp",
                "substats": [], "location": "", "lock": false
            }]
        }"#;
        let req: LockManageRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.lock.len(), 1);
        assert!(req.unlock.is_empty());
    }

    #[test]
    fn test_lock_manage_request_with_unactivated_substats() {
        let json = r#"{
            "lock": [{
                "setKey": "GladiatorsFinale", "slotKey": "flower",
                "rarity": 5, "level": 0, "mainStatKey": "hp",
                "substats": [
                    {"key": "critRate_", "value": 3.9},
                    {"key": "critDMG_", "value": 7.8},
                    {"key": "atk_", "value": 5.8}
                ],
                "unactivatedSubstats": [
                    {"key": "def", "value": 23.0}
                ],
                "location": "", "lock": false
            }]
        }"#;
        let req: LockManageRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.lock[0].substats.len(), 3);
        assert_eq!(req.lock[0].unactivated_substats.len(), 1);
        assert_eq!(req.lock[0].unactivated_substats[0].key, "def");
    }
}
