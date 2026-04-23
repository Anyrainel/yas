use serde::{Deserialize, Serialize};

use crate::scanner::common::models::GoodArtifact;

/// Scan request: which categories to scan remotely.
/// At least one target must be true.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanRequest {
    #[serde(default)]
    pub characters: bool,
    #[serde(default)]
    pub weapons: bool,
    #[serde(default)]
    pub artifacts: bool,
}

/// Equip/unequip request: a list of equip instructions.
/// Each pairs an artifact (GOOD v3 format) with a target location.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquipRequest {
    pub equip: Vec<EquipInstruction>,
}

/// A single equip instruction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquipInstruction {
    /// The artifact to equip/unequip, in GOOD v3 format.
    pub artifact: GoodArtifact,
    /// Target character key (e.g. "RaidenShogun"), or "" to unequip.
    pub location: String,
}

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
    pub id: String,
    pub status: InstructionStatus,
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

/// Linear progress for single-phase jobs (manage, equip).
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

/// State of one scan category.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PhaseState {
    /// Category was requested but hasn't started yet.
    Pending,
    /// Category is currently being scanned.
    Running,
    /// Category finished successfully.
    Complete,
    /// Category stopped before finishing (user abort, error, or never reached).
    Aborted,
}

/// Progress of one scan category.
///
/// For weapons/artifacts `total` is the backpack item count (known once the
/// bag opens). For characters the game doesn't expose a total, so `total`
/// stays equal to `completed` and the UI should render as an indeterminate
/// counter rather than a percentage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseProgress {
    pub completed: usize,
    pub total: usize,
    pub state: PhaseState,
}

impl PhaseProgress {
    pub fn pending() -> Self {
        Self { completed: 0, total: 0, state: PhaseState::Pending }
    }
}

/// Per-category progress for a scan job. `None` means the client didn't
/// request that category; otherwise the struct tracks its lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScanProgress {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub characters: Option<PhaseProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weapons: Option<PhaseProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<PhaseProgress>,
}

/// Shared state for an async job, polled via GET /status.
///
/// `progress` is populated for manage/equip jobs (single-phase).
/// `scan_progress` is populated for scan jobs (per-category).
/// Exactly one of the two is `Some` while the job is running.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobState {
    pub state: JobPhase,
    #[serde(rename = "jobId", skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<JobProgress>,
    #[serde(rename = "scanProgress", skip_serializing_if = "Option::is_none")]
    pub scan_progress: Option<ScanProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ManageResult>,
}

impl JobState {
    pub fn idle() -> Self {
        Self {
            state: JobPhase::Idle, job_id: None,
            progress: None, scan_progress: None, result: None,
        }
    }

    /// Running state for manage/equip — linear progress.
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
            scan_progress: None,
            result: None,
        }
    }

    /// Running state for scan — per-category progress. Only categories in
    /// `requested` get a `Pending` slot; unrequested ones stay `None`.
    pub fn running_scan(
        job_id: String,
        scan_characters: bool,
        scan_weapons: bool,
        scan_artifacts: bool,
    ) -> Self {
        let sp = ScanProgress {
            characters: if scan_characters { Some(PhaseProgress::pending()) } else { None },
            weapons: if scan_weapons { Some(PhaseProgress::pending()) } else { None },
            artifacts: if scan_artifacts { Some(PhaseProgress::pending()) } else { None },
        };
        Self {
            state: JobPhase::Running,
            job_id: Some(job_id),
            progress: None,
            scan_progress: Some(sp),
            result: None,
        }
    }

    pub fn completed(job_id: String, result: ManageResult) -> Self {
        Self {
            state: JobPhase::Completed,
            job_id: Some(job_id),
            progress: None,
            scan_progress: None,
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
                    let cid = escape_json_string(&p.current_id);
                    let phase = escape_json_string(&p.phase);
                    format!(
                        r#"{{"state":"running","jobId":"{}","progress":{{"completed":{},"total":{},"currentId":"{}","phase":"{}"}}}}"#,
                        job_id, p.completed, p.total, cid, phase
                    )
                } else if let Some(ref sp) = self.scan_progress {
                    let body = serde_json::to_string(sp)
                        .unwrap_or_else(|_| "{}".to_string());
                    format!(
                        r#"{{"state":"running","jobId":"{}","scanProgress":{}}}"#,
                        job_id, body
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

/// Minimal JSON string escaping for the subset of chars that can appear in
/// `current_id` / `phase` (IDs, Chinese/English phase names). Covers quotes,
/// backslashes, and control characters — enough for our fixed inputs.
fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
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

    #[test]
    fn test_equip_request_deser_equip_to_character() {
        let json = r#"{
            "equip": [{
                "artifact": {
                    "setKey": "GladiatorsFinale",
                    "slotKey": "flower",
                    "rarity": 5,
                    "level": 20,
                    "mainStatKey": "hp",
                    "substats": [{"key": "critRate_", "value": 3.9}],
                    "location": "Furina",
                    "lock": true
                },
                "location": "RaidenShogun"
            }]
        }"#;
        let req: EquipRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.equip.len(), 1);
        assert_eq!(req.equip[0].artifact.set_key, "GladiatorsFinale");
        assert_eq!(req.equip[0].location, "RaidenShogun");
    }

    #[test]
    fn test_equip_request_deser_unequip() {
        let json = r#"{
            "equip": [{
                "artifact": {
                    "setKey": "WanderersTroupe",
                    "slotKey": "plume",
                    "rarity": 5,
                    "level": 16,
                    "mainStatKey": "atk",
                    "substats": [{"key": "hp", "value": 508.0}],
                    "location": "Furina",
                    "lock": false
                },
                "location": ""
            }]
        }"#;
        let req: EquipRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.equip.len(), 1);
        assert_eq!(req.equip[0].location, "");
        assert_eq!(req.equip[0].artifact.location, "Furina");
    }

    #[test]
    fn test_equip_request_deser_empty_list() {
        let json = r#"{"equip": []}"#;
        let req: EquipRequest = serde_json::from_str(json).unwrap();
        assert!(req.equip.is_empty());
    }
}
