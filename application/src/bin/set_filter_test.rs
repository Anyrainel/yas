/// Test binary: randomly pick artifacts from a scan export, navigate to the
/// artifact selection view, apply set filter, and verify each artifact can be
/// found in the grid.
///
/// Stop mechanism: right-click the mouse to cancel at any time.
///
/// Usage:
///   cargo run --release --bin set_filter_test --features dev-tools
///   cargo run --release --bin set_filter_test --features dev-tools -- --count 100
///   cargo run --release --bin set_filter_test --features dev-tools -- --json path/to/export.json
///
/// Default JSON: target/release/good_export_2026-04-01_06-20-21.json

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use image::RgbImage;
use log::{error, info, warn};
use rand::seq::SliceRandom;
use rand::thread_rng;

use yas::game_info::GameInfoBuilder;
use yas::utils;

use yas_genshin::manager::ui_actions;
use yas_genshin::scanner::common::game_controller::GenshinGameController;
use yas_genshin::scanner::common::mappings::{MappingManager, NameOverrides};
use yas_genshin::scanner::common::models::{GoodArtifact, GoodExport};
use yas_genshin::scanner::common::ocr_factory;

fn save_image(img: &RgbImage, path: &str) {
    let (w, h) = (img.width(), img.height());
    let saved = if w > 1920 {
        let scale = 1920.0 / w as f64;
        let new_w = 1920;
        let new_h = (h as f64 * scale) as u32;
        image::imageops::resize(img, new_w, new_h, image::imageops::FilterType::Lanczos3)
    } else {
        image::ImageBuffer::from_raw(w, h, img.as_raw().clone()).unwrap()
    };
    if let Err(e) = saved.save(path) {
        error!("Failed to save image {}: {}", path, e);
    }
}

/// Check RMB cancel and bail if pressed.
macro_rules! check_cancel {
    ($ctrl:expr) => {
        if $ctrl.check_rmb() {
            info!("=== Cancelled by right-click ===");
            bail!("Cancelled by user (right-click)");
        }
    };
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("info"),
    )
    .format_timestamp(None)
    .init();

    // Parse args
    let args: Vec<String> = std::env::args().collect();
    let count: usize = args
        .windows(2)
        .find(|w| w[0] == "--count")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(10);
    let json_path: PathBuf = args
        .windows(2)
        .find(|w| w[0] == "--json")
        .map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(|| PathBuf::from(r"F:\Codes\genshin\genshin_export_2026-04-01_17-24.json"));
    // --keep-images: don't clean debug dir between iterations
    let keep_images = args.iter().any(|a| a == "--keep-images");
    let test_filters = args.iter().any(|a| a == "--test-filters");
    // --debug-sets Set1,Set2,... — only test these sets with debug images
    let debug_sets: Option<Vec<String>> = args
        .windows(2)
        .find(|w| w[0] == "--debug-sets")
        .map(|w| w[1].split(',').map(|s| s.to_string()).collect());

    let out_dir = Path::new("debug_images/set_filter_test");
    std::fs::create_dir_all(out_dir)?;

    let dump_grid = args.iter().any(|a| a == "--dump-grid");

    if test_filters || debug_sets.is_some() {
        return run_filter_test(debug_sets.as_deref());
    }

    if dump_grid {
        let set_key = args
            .windows(2)
            .find(|w| w[0] == "--set")
            .map(|w| w[1].clone());
        return run_dump_grid(set_key.as_deref());
    }

    // Load artifact data
    info!("Loading artifacts from: {}", json_path.display());
    let json_str = std::fs::read_to_string(&json_path)?;
    let export: GoodExport = serde_json::from_str(&json_str)?;
    let artifacts = export.artifacts.unwrap_or_default();
    if artifacts.is_empty() {
        bail!("No artifacts in {}", json_path.display());
    }
    info!("Loaded {} artifacts", artifacts.len());

    // Filter to rarity >= 4 AND locked AND not equipped on a character.
    let eligible: Vec<&GoodArtifact> = artifacts
        .iter()
        .filter(|a| a.rarity >= 4 && a.lock && a.location.is_empty())
        .collect();
    info!(
        "Eligible artifacts (rarity >= 4, locked, unequipped): {}",
        eligible.len()
    );

    let mut rng = thread_rng();
    let mut selection: Vec<&GoodArtifact> = eligible.clone();
    selection.shuffle(&mut rng);
    let test_artifacts: Vec<&GoodArtifact> = selection.into_iter().take(count).collect();
    info!("Will test {} random artifacts", test_artifacts.len());
    info!("Right-click at any time to stop.");

    // Build game info
    let game_info = GameInfoBuilder::new()
        .add_local_window_name("\u{539F}\u{795E}")
        .add_local_window_name("Genshin Impact")
        .build()?;
    info!(
        "Window: left={}, top={}, w={}, h={}",
        game_info.window.left,
        game_info.window.top,
        game_info.window.width,
        game_info.window.height
    );

    let mut ctrl = GenshinGameController::new(game_info)?;
    let mappings = MappingManager::new(&NameOverrides::default())?;
    let ocr = ocr_factory::create_ocr_model("ppocrv4")?;

    // Navigate to artifact selection view.
    // Use return_to_main_ui (Paimon detection) — no ESC spam needed.
    info!("=== Navigating to artifact selection view ===");
    ctrl.focus_game_window();
    utils::sleep(300);
    ctrl.return_to_main_ui(8);
    utils::sleep(500);
    check_cancel!(ctrl);

    info!("Opening character screen...");
    ui_actions::ensure_character_screen(&mut ctrl, ocr.as_ref(), &mappings)?;
    check_cancel!(ctrl);

    info!("Clicking artifact menu...");
    ctrl.click_at(160.0, 293.0);
    utils::sleep(800);
    check_cancel!(ctrl);

    info!("Clicking replace button...");
    ctrl.click_at(1720.0, 1010.0);
    utils::sleep(1500);
    check_cancel!(ctrl);

    if let Ok(img) = ctrl.capture_game() {
        save_image(&img, "debug_images/nav_screenshot.png");
    }

    info!("=== Starting artifact find tests ===");

    let mut passed = 0;
    let grid_debug_dir = out_dir.join("grid");
    std::fs::create_dir_all(&grid_debug_dir)?;

    for (i, artifact) in test_artifacts.iter().enumerate() {
        check_cancel!(ctrl);

        // Clean grid debug dir before each attempt
        clean_debug_dir(&grid_debug_dir);

        info!(
            "--- [{}/{}] set={} slot={} lv={} main={} ---",
            i + 1,
            test_artifacts.len(),
            artifact.set_key,
            artifact.slot_key,
            artifact.level,
            artifact.main_stat_key
        );

        // Apply set filter (clears any previous filter automatically).
        let filter_ok =
            ui_actions::apply_set_filter(&mut ctrl, &artifact.set_key, &mappings, ocr.as_ref())?;
        if !filter_ok {
            warn!(
                "[{}/{}] Set filter failed for '{}' \u{2014} skipping",
                i + 1,
                test_artifacts.len(),
                artifact.set_key
            );
            continue;
        }
        check_cancel!(ctrl);

        // Click slot tab
        ui_actions::click_slot_tab(&mut ctrl, &artifact.slot_key)?;
        check_cancel!(ctrl);

        // Find artifact in grid with debug collection
        let mut debug_cells = Vec::new();
        let found = ui_actions::find_artifact_in_grid_debug(
            &mut ctrl, artifact, ocr.as_ref(), &mappings, false, &mut debug_cells,
        )?;

        if found {
            info!(
                "[PASS {}/{}] Found: set={} slot={} lv={}",
                passed + 1,
                i + 1,
                artifact.set_key,
                artifact.slot_key,
                artifact.level
            );
            passed += 1;
            // Clean debug dir on success
            clean_debug_dir(&grid_debug_dir);
        } else {
            // === FAILURE: save all debug images and OCR details, then exit ===
            error!(
                "[FAIL] NOT FOUND after {} passes: set={} slot={} lv={} main={}",
                passed,
                artifact.set_key,
                artifact.slot_key,
                artifact.level,
                artifact.main_stat_key
            );
            for (j, sub) in artifact.substats.iter().enumerate() {
                error!("  target sub[{}]: {} = {}", j, sub.key, sub.value);
            }
            for (j, sub) in artifact.unactivated_substats.iter().enumerate() {
                error!("  target unsub[{}]: {} = {} (inactive)", j, sub.key, sub.value);
            }

            // Save panel images and OCR details for every cell
            let mut report = String::new();
            use std::fmt::Write;
            let _ = writeln!(report, "=== FAILURE REPORT ===");
            let _ = writeln!(report, "Target: set={} slot={} lv={} main={}",
                artifact.set_key, artifact.slot_key, artifact.level, artifact.main_stat_key);
            for sub in &artifact.substats {
                let _ = writeln!(report, "  target sub: {} = {}", sub.key, sub.value);
            }
            for sub in &artifact.unactivated_substats {
                let _ = writeln!(report, "  target unsub: {} = {} (inactive)", sub.key, sub.value);
            }
            let _ = writeln!(report, "\n=== CELLS SCANNED ({}) ===", debug_cells.len());

            for cell in &debug_cells {
                let _ = writeln!(report, "\n--- page={} row={} col={} ---", cell.page, cell.row, cell.col);
                let _ = writeln!(report, "level: OCR='{}' parsed={} full_ocr={} match={:?}",
                    cell.level_text, cell.level, cell.full_ocr, cell.match_result);
                let _ = writeln!(report, "{}", cell.ocr_details);

                // Save panel image
                let img_path = grid_debug_dir.join(
                    format!("p{}_r{}_c{}.png", cell.page, cell.row, cell.col)
                );
                if let Err(e) = cell.panel_image.save(&img_path) {
                    error!("Failed to save {}: {}", img_path.display(), e);
                }
            }

            // Save report
            let report_path = grid_debug_dir.join("report.txt");
            if let Err(e) = std::fs::write(&report_path, &report) {
                error!("Failed to save report: {}", e);
            }

            // Final screenshot
            if let Ok(img) = ctrl.capture_game() {
                save_image(&img, &grid_debug_dir.join("final_screen.png").to_string_lossy());
            }

            info!("Debug images saved to: {}", grid_debug_dir.display());
            info!("Report saved to: {}", report_path.display());
            bail!(
                "Artifact not found: set={} slot={} lv={} ({} passed before failure)",
                artifact.set_key, artifact.slot_key, artifact.level, passed
            );
        }
    }

    // Return to main world
    info!("=== Returning to main world ===");
    ctrl.key_press(enigo::Key::Escape);
    utils::sleep(500);
    ctrl.key_press(enigo::Key::Escape);
    utils::sleep(500);
    ctrl.key_press(enigo::Key::Escape);
    utils::sleep(500);

    info!("========================================");
    info!(
        "ALL PASSED: {}/{} artifacts found successfully!",
        passed, passed
    );
    info!("========================================");
    Ok(())
}

fn clean_debug_dir(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

// ================================================================
// Filter detection test (--test-filters)
// ================================================================

use yas_genshin::manager::ui_actions::{
    FILTER_FUNNEL_X, FILTER_FUNNEL_Y, FILTER_CLEAR_X, FILTER_CLEAR_Y,
    FILTER_CLOSE_X, FILTER_CLOSE_Y,
};

fn run_filter_test(only_sets: Option<&[String]>) -> Result<()> {
    let game_info = GameInfoBuilder::new()
        .add_local_window_name("\u{539F}\u{795E}")
        .add_local_window_name("Genshin Impact")
        .build()?;
    let mut ctrl = GenshinGameController::new(game_info)?;
    let mappings = MappingManager::new(&NameOverrides::default())?;
    let ocr = ocr_factory::create_ocr_model("ppocrv4")?;

    // Collect set keys to test
    let mut all_set_keys: Vec<String> = mappings
        .artifact_set_map
        .values()
        .cloned()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    all_set_keys.sort();

    let test_keys: Vec<String> = match only_sets {
        Some(subset) => {
            // Filter to only requested sets
            all_set_keys.iter()
                .filter(|k| subset.iter().any(|s| s == *k))
                .cloned()
                .collect()
        }
        None => all_set_keys.clone(),
    };

    let use_debug = only_sets.is_some();
    info!("Testing detection of {} artifact sets{}", test_keys.len(),
        if use_debug { " (with debug images)" } else { "" });

    // Navigate to selection view
    info!("=== Navigating to artifact selection view ===");
    ctrl.focus_game_window();
    utils::sleep(300);
    ctrl.return_to_main_ui(8);
    utils::sleep(500);
    check_cancel!(ctrl);

    ui_actions::ensure_character_screen(&mut ctrl, ocr.as_ref(), &mappings)?;
    check_cancel!(ctrl);

    ctrl.click_at(160.0, 293.0); // artifact menu
    utils::sleep(800);
    ctrl.click_at(1720.0, 1010.0); // replace button
    utils::sleep(1500);
    check_cancel!(ctrl);

    // Open filter panel
    ctrl.click_at(FILTER_FUNNEL_X, FILTER_FUNNEL_Y);
    utils::sleep(1000);

    // Clear
    ctrl.click_at(FILTER_CLEAR_X, FILTER_CLEAR_Y);
    utils::sleep(300);

    let mut passed = 0;
    let mut failed_sets: Vec<String> = Vec::new();

    for set_key in &test_keys {
        check_cancel!(ctrl);

        let cn_name = mappings
            .artifact_set_map
            .iter()
            .find(|(_, v)| v.as_str() == set_key)
            .map(|(k, _)| k.clone())
            .unwrap_or_default();

        let found = if use_debug {
            // Use debug variant that saves annotated screenshots on failure
            let prefix = format!("debug_images/set_filter_test/{}", set_key);
            ui_actions::find_set_in_filter_panel_debug(
                &mut ctrl, ocr.as_ref(), set_key, &mappings, &prefix,
            )?
        } else {
            ui_actions::find_set_in_filter_panel(
                &mut ctrl, ocr.as_ref(), set_key, &mappings,
            )?
        };

        match found {
            Some(_) => {
                info!("[PASS] {} ({})", set_key, cn_name);
                passed += 1;
            }
            None => {
                error!("[FAIL] {} ({}) \u{2014} not found in filter list", set_key, cn_name);
                failed_sets.push(format!("{} ({})", set_key, cn_name));
            }
        }
    }

    // Close filter panel
    ctrl.click_at(FILTER_CLOSE_X, FILTER_CLOSE_Y);
    utils::sleep(500);

    info!("========================================");
    info!(
        "Filter detection: {}/{} passed, {} failed",
        passed,
        test_keys.len(),
        failed_sets.len()
    );
    for s in &failed_sets {
        error!("  FAILED: {}", s);
    }
    info!("========================================");

    if !failed_sets.is_empty() {
        bail!("{} set(s) not detected", failed_sets.len());
    }
    Ok(())
}

// ================================================================
// Dump grid screenshots (--dump-grid)
// ================================================================

/// Navigate to artifact selection view and save full screen capture per cell click.
/// Saves to debug_images/set_filter_test/screens/p{page}_r{row}_c{col}.png
/// Optionally applies a set filter via `--set <SetKey>`.
fn run_dump_grid(set_key: Option<&str>) -> Result<()> {
    let game_info = GameInfoBuilder::new()
        .add_local_window_name("\u{539F}\u{795E}")
        .add_local_window_name("Genshin Impact")
        .build()?;
    let mut ctrl = GenshinGameController::new(game_info)?;
    let mappings = MappingManager::new(&NameOverrides::default())?;
    let ocr = ocr_factory::create_ocr_model("ppocrv4")?;

    let out_dir = Path::new("debug_images/set_filter_test/screens");
    std::fs::create_dir_all(out_dir)?;
    clean_debug_dir(out_dir);

    // Navigate to selection view
    info!("=== Navigating to artifact selection view ===");
    ctrl.focus_game_window();
    utils::sleep(300);
    ctrl.return_to_main_ui(8);
    utils::sleep(500);
    check_cancel!(ctrl);

    ui_actions::ensure_character_screen(&mut ctrl, ocr.as_ref(), &mappings)?;
    check_cancel!(ctrl);

    ctrl.click_at(160.0, 293.0); // artifact menu
    utils::sleep(800);
    ctrl.click_at(1720.0, 1010.0); // replace button
    utils::sleep(1500);
    check_cancel!(ctrl);

    // Apply set filter if requested
    if let Some(key) = set_key {
        info!("Applying set filter: {}", key);
        let ok = ui_actions::apply_set_filter(&mut ctrl, key, &mappings, ocr.as_ref())?;
        if !ok {
            bail!("Failed to apply set filter for '{}'", key);
        }
        check_cancel!(ctrl);
    }

    // Grid constants (same as ui_actions)
    let cols = 4usize;
    let rows = 5usize;
    let first_x = 89.0;
    let first_y = 130.0;
    let offset_x = 141.0;
    let offset_y = 167.0;

    let slots = ["flower", "plume", "sands", "goblet", "circlet"];
    let mut count = 0;

    for slot in &slots {
        check_cancel!(ctrl);
        info!("=== Slot: {} ===", slot);
        ui_actions::click_slot_tab(&mut ctrl, slot)?;

        for row in 0..rows {
            for col in 0..cols {
                check_cancel!(ctrl);

                let x = first_x + col as f64 * offset_x;
                let y = first_y + row as f64 * offset_y;
                ctrl.click_at(x, y);
                utils::sleep(150);

                if let Ok(img) = ctrl.capture_game() {
                    let path = out_dir.join(format!("{}_r{}_c{}.png", slot, row, col));
                    save_image(&img, &path.to_string_lossy());
                    count += 1;
                    info!("Saved {} ({} row={} col={})", path.display(), slot, row, col);
                }
            }
        }
    }

    info!("Saved {} full screen captures to {}", count, out_dir.display());
    Ok(())
}
