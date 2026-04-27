/// Test binary: replay an equip request JSON against the live game.
///
/// Reads a saved equip_*.json file (from the log/ directory) and runs the
/// equip manager against the game, printing results.
///
/// Usage:
///   cargo run --bin equip_replay --features dev-tools -- --json path/to/equip_*.json
///   cargo run --bin equip_replay --features dev-tools -- --json path/to/equip_*.json --dump-images
///
/// Right-click to cancel at any time.

use std::path::PathBuf;

use anyhow::Result;
use log::info;

use yas::cancel::CancelToken;
use yas::game_info::GameInfoBuilder;
use yas::utils;

use genshin_scanner::cli::load_config_or_default;
use genshin_scanner::manager::models::EquipRequest;
use genshin_scanner::manager::orchestrator::ArtifactManager;
use genshin_scanner::scanner::common::game_controller::GenshinGameController;
use genshin_scanner::scanner::common::mappings::MappingManager;

fn main() -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .format_timestamp(None)
    .init();

    let args: Vec<String> = std::env::args().collect();
    let json_path: PathBuf = args
        .windows(2)
        .find(|w| w[0] == "--json")
        .map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(|| {
            // Find newest equip_*.json in target/debug/log/
            find_newest_equip_json().unwrap_or_else(|| PathBuf::from("equip.json"))
        });
    let dump_images = args.iter().any(|a| a == "--dump-images");

    info!("Loading equip request from: {}", json_path.display());
    let json_str = std::fs::read_to_string(&json_path)?;
    let request: EquipRequest = serde_json::from_str(&json_str)?;
    info!("Loaded {} equip instructions", request.equip.len());

    // Summarize by character
    let mut char_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for instr in &request.equip {
        *char_counts.entry(&instr.location).or_default() += 1;
    }
    let mut chars: Vec<(&&str, &usize)> = char_counts.iter().collect();
    chars.sort_by_key(|(k, _)| **k);
    for (name, count) in &chars {
        info!("  {}: {} artifacts", name, count);
    }

    let game_info = GameInfoBuilder::new()
        .add_local_window_name("\u{539F}\u{795E}")
        .add_local_window_name("Genshin Impact")
        .build()?;
    info!(
        "Window: left={}, top={}, w={}, h={}",
        game_info.window.left, game_info.window.top,
        game_info.window.width, game_info.window.height
    );

    let mut ctrl = GenshinGameController::new(game_info)?;
    let user_config = load_config_or_default();
    let overrides = user_config.to_overrides();
    let mappings = MappingManager::new(&overrides)?;

    let manager = ArtifactManager::new(
        std::sync::Arc::new(mappings),
        "ppocrv4".to_string(),
        "ppocrv4".to_string(),
        user_config.inv_scroll_delay,
        false,
        dump_images,
    );

    let cancel = CancelToken::new();

    info!("=== Starting equip replay (right-click to cancel) ===");
    utils::sleep(1000);

    let result = manager.execute_equip(&mut ctrl, request, None, cancel);

    info!("=== Results ===");
    info!(
        "Total: {}, Success: {}, Already correct: {}, Not found: {}, Errors: {}, Aborted: {}",
        result.summary.total, result.summary.success, result.summary.already_correct,
        result.summary.not_found, result.summary.errors, result.summary.aborted,
    );

    // Print failures
    for r in &result.results {
        if r.status != genshin_scanner::manager::models::InstructionStatus::Success
            && r.status != genshin_scanner::manager::models::InstructionStatus::AlreadyCorrect
        {
            info!("  {} => {:?}", r.id, r.status);
        }
    }

    // Save result
    let result_path = json_path.with_extension("result.json");
    let result_json = serde_json::to_string_pretty(&result)?;
    std::fs::write(&result_path, &result_json)?;
    info!("Result saved to: {}", result_path.display());

    Ok(())
}

fn find_newest_equip_json() -> Option<PathBuf> {
    let log_dir = PathBuf::from("log");
    if !log_dir.is_dir() {
        return None;
    }
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(&log_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("equip_") && name.ends_with(".json") {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if newest.as_ref().map_or(true, |(t, _)| modified > *t) {
                            newest = Some((modified, entry.path()));
                        }
                    }
                }
            }
        }
    }
    newest.map(|(_, p)| p)
}
