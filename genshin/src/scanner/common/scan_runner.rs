//! Shared scan orchestration used by the GUI/CLI scanner and the HTTP `/scan` API.
//!
//! The individual scanners own OCR and navigation inside each category. This
//! module owns the cross-category contract: phase order, per-phase progress
//! keys, cancellation behavior, and the weapon-to-artifact skip-open handoff.

use std::sync::Arc;

use anyhow::Result;
use yas::{log_info, log_warn};

use crate::cli::{GoodScannerApplication, GoodUserConfig, ScanCoreConfig};
use crate::scanner::artifact::GoodArtifactScanner;
use crate::scanner::character::GoodCharacterScanner;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::MappingManager;
use crate::scanner::common::models::{GoodArtifact, GoodCharacter, GoodWeapon};
use crate::scanner::common::ocr_pool::SharedOcrPools;
use crate::scanner::common::progress::ProgressFn;
use crate::scanner::weapon::GoodWeaponScanner;

/// Result of a single scan phase.
///
/// A phase is `Complete` only if the caller is allowed to publish/export its
/// data. Any abort, error, or skipped start is represented explicitly so HTTP
/// caches and GUI exports do not infer completeness from empty vectors.
pub enum ScanPhaseResult<T> {
    /// Phase was not requested by the caller.
    NotAttempted,
    /// Phase was requested but did not finish or should not be published.
    Incomplete,
    /// Phase finished with publishable data.
    Complete(Vec<T>),
}

impl<T> ScanPhaseResult<T> {
    pub fn into_complete(self) -> Option<Vec<T>> {
        match self {
            Self::Complete(data) => Some(data),
            Self::NotAttempted | Self::Incomplete => None,
        }
    }
}

/// Result of a scan execution. Each category reports Complete/Incomplete/NotAttempted.
pub struct ScanRunResult {
    pub characters: ScanPhaseResult<GoodCharacter>,
    pub weapons: ScanPhaseResult<GoodWeapon>,
    pub artifacts: ScanPhaseResult<GoodArtifact>,
}

/// Whether one failed phase should abort the whole scan or only mark that phase incomplete.
#[derive(Clone, Copy)]
pub enum ScanFailurePolicy {
    StopOnError,
    ContinueOnError,
}

/// Runtime behavior that differs between local exports and HTTP scan jobs.
#[derive(Clone, Copy)]
pub struct ScanRunOptions {
    /// If true, a cancellation error in the active phase becomes an empty
    /// complete phase so local GUI scans can still export already-collected data.
    pub save_on_cancel: bool,
    /// If true, a phase that returns data after cancellation is still publishable.
    /// GUI/CLI exports use this to preserve partial files; HTTP `/scan` keeps
    /// cancelled jobs incomplete so clients do not consume stale cache entries.
    pub accept_cancelled_success: bool,
    pub failure_policy: ScanFailurePolicy,
}

/// Execute the requested scan phases with shared scanner setup and phase semantics.
pub fn run_scan_phases(
    ctrl: &mut GenshinGameController,
    mappings: Arc<MappingManager>,
    pools: Arc<SharedOcrPools>,
    user_config: &GoodUserConfig,
    config: &ScanCoreConfig,
    progress_fn: Option<&ProgressFn<'_>>,
    status_fn: Option<&dyn Fn(&str)>,
    cancel_token: yas::cancel::CancelToken,
    options: ScanRunOptions,
) -> Result<ScanRunResult> {
    let scanner_config = config.to_scanner_config();

    ctrl.focus_game_window();
    ctrl.set_cancel_token(cancel_token.clone());

    let report = |msg: &str| {
        if let Some(f) = status_fn {
            f(msg);
        }
    };

    let chars_progress = |c: usize, t: usize, id: &str, _phase: &str| {
        if let Some(outer) = progress_fn {
            outer(c, t, id, "characters");
        }
    };
    let weapons_progress = |c: usize, t: usize, id: &str, _phase: &str| {
        if let Some(outer) = progress_fn {
            outer(c, t, id, "weapons");
        }
    };
    let artifacts_progress = |c: usize, t: usize, id: &str, _phase: &str| {
        if let Some(outer) = progress_fn {
            outer(c, t, id, "artifacts");
        }
    };

    let mut characters = ScanPhaseResult::NotAttempted;
    let mut weapons = ScanPhaseResult::NotAttempted;
    let mut artifacts = ScanPhaseResult::NotAttempted;

    if config.scan_characters {
        characters = if cancel_token.is_cancelled() {
            ScanPhaseResult::Incomplete
        } else {
            report("扫描角色 / Scanning characters...");
            log_info!("扫描角色...", "Scanning characters...");
            let cfg = GoodScannerApplication::make_char_config(&scanner_config, user_config);
            let scan_result = match GoodCharacterScanner::new(cfg, mappings.clone()) {
                Ok(scanner) => scanner.scan(ctrl, 0, &pools, Some(&chars_progress)),
                Err(e) => Err(e),
            };
            let phase = phase_result(scan_result, &cancel_token, options, "character")?;
            if matches!(phase, ScanPhaseResult::Complete(_)) && !cancel_token.is_cancelled() {
                ctrl.return_to_main_ui(4);
            }
            phase
        };
    }

    if config.scan_weapons && !cancel_token.is_cancelled() {
        report("扫描武器 / Scanning weapons...");
        log_info!("扫描武器...", "Scanning weapons...");
        let cfg = GoodScannerApplication::make_weapon_config(&scanner_config, user_config);
        let scan_result = match GoodWeaponScanner::new(cfg, mappings.clone()) {
            Ok(scanner) => scanner.scan(ctrl, false, 0, &pools, Some(&weapons_progress)),
            Err(e) => Err(e),
        };
        weapons = phase_result(scan_result, &cancel_token, options, "weapon")?;
    }

    if config.scan_artifacts && !cancel_token.is_cancelled() {
        report("扫描圣遗物 / Scanning artifacts...");
        log_info!("扫描圣遗物...", "Scanning artifacts...");
        let cfg = GoodScannerApplication::make_artifact_config(&scanner_config, user_config);
        let skip_open = matches!(weapons, ScanPhaseResult::Complete(_));
        let scan_result = match GoodArtifactScanner::new(cfg, mappings.clone()) {
            Ok(scanner) => scanner.scan(ctrl, skip_open, 0, &pools, Some(&artifacts_progress)),
            Err(e) => Err(e),
        };
        artifacts = phase_result(scan_result, &cancel_token, options, "artifact")?;
    }

    Ok(ScanRunResult { characters, weapons, artifacts })
}

fn phase_result<T>(
    result: Result<Vec<T>>,
    cancel_token: &yas::cancel::CancelToken,
    options: ScanRunOptions,
    phase_name: &str,
) -> Result<ScanPhaseResult<T>> {
    match result {
        Ok(data) if options.accept_cancelled_success || !cancel_token.is_cancelled() => {
            Ok(ScanPhaseResult::Complete(data))
        }
        Ok(_) => Ok(ScanPhaseResult::Incomplete),
        Err(e) if options.save_on_cancel && cancel_token.is_cancelled() => {
            log_info!("阶段被用户中断: {}", "Phase aborted by user: {}", e);
            Ok(ScanPhaseResult::Complete(Vec::new()))
        }
        Err(e) => {
            log_warn!("[scan] {}阶段失败: {:#}", "[scan] {} phase failed: {:#}", phase_name, e);
            match options.failure_policy {
                ScanFailurePolicy::StopOnError => Err(e),
                ScanFailurePolicy::ContinueOnError => Ok(ScanPhaseResult::Incomplete),
            }
        }
    }
}
