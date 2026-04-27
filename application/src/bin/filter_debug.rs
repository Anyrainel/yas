/// Debug binary: Navigate to the artifact filter panel and capture OCR regions.
///
/// Usage: cargo run --release --bin filter_debug
///
/// Steps:
/// 1. Press Escape to return to main world
/// 2. Press C to open character screen
/// 3. Click 圣遗物 menu
/// 4. Click 替换 button
/// 5. Click filter funnel
/// 6. Capture and save OCR region crops + binarized versions

use std::path::Path;

use anyhow::Result;
use image::RgbImage;
use log::info;

use yas::game_info::{GameInfo, GameInfoBuilder};
use yas::utils;

use genshin_scanner::scanner::common::game_controller::GenshinGameController;

fn save_image(img: &RgbImage, path: &str) {
    img.save(path).unwrap();
    info!("Saved: {}", path);
}

fn binarize(img: &RgbImage, threshold: u32) -> RgbImage {
    let mut out = img.clone();
    for pixel in out.pixels_mut() {
        let brightness = (pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32) / 3;
        if brightness > threshold {
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
        } else {
            pixel[0] = 255;
            pixel[1] = 255;
            pixel[2] = 255;
        }
    }
    out
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .init();

    let out_dir = Path::new("D:/Codes/genshin/yas/calibration_screenshots");
    std::fs::create_dir_all(out_dir)?;

    // Build game info
    let game_info = GameInfoBuilder::new()
        .add_local_window_name("原神")
        .add_local_window_name("Genshin Impact")
        .build()?;

    info!("Window: left={}, top={}, w={}, h={}",
        game_info.window.left, game_info.window.top,
        game_info.window.width, game_info.window.height);

    let mut ctrl = GenshinGameController::new(game_info)?;

    // Step 1: Return to main UI
    info!("Step 1: Returning to main UI...");
    ctrl.return_to_main_ui(4);
    utils::sleep(500);

    // Step 2: Press C to open character screen
    info!("Step 2: Opening character screen...");
    ctrl.focus_game_window();
    ctrl.key_press(enigo::Key::Layout('c'));
    utils::sleep(1500);

    // Verify: take screenshot
    let img = ctrl.capture_game()?;
    save_image(&img, &out_dir.join("fd_step2_char_screen.png").to_string_lossy());

    // Step 3: Click 圣遗物 menu (160, 293)
    info!("Step 3: Clicking 圣遗物 menu at (160, 293)...");
    ctrl.click_at(160.0, 293.0);
    utils::sleep(1500);

    let img = ctrl.capture_game()?;
    save_image(&img, &out_dir.join("fd_step3_artifact_menu.png").to_string_lossy());

    // Step 4: Click 替换 button (1720, 1010)
    info!("Step 4: Clicking 替换 at (1720, 1010)...");
    ctrl.click_at(1720.0, 1010.0);
    utils::sleep(2500);

    let img = ctrl.capture_game()?;
    save_image(&img, &out_dir.join("fd_step4_replace.png").to_string_lossy());

    // Step 5: Click filter funnel (110, 1005)
    info!("Step 5: Clicking filter funnel at (110, 1005)...");
    ctrl.click_at(110.0, 1005.0);
    utils::sleep(2000);

    // Step 6: Full screenshot of filter panel
    let img = ctrl.capture_game()?;
    save_image(&img, &out_dir.join("fd_step5_filter_panel.png").to_string_lossy());

    // Step 7: Capture OCR regions for first 3 rows
    info!("Step 6: Capturing OCR regions...");
    for row in 0..3 {
        let y_center = 236.5 + row as f64 * 81.5;
        let text_y = y_center - 17.5;

        for (col_name, x_start) in [("left", 155.0), ("right", 615.0)] {
            // Capture region
            let crop = ctrl.capture_region(x_start, text_y, 280.0, 35.0)?;
            let base = format!("fd_row{}_{}", row, col_name);

            save_image(&crop, &out_dir.join(format!("{}.png", base)).to_string_lossy());

            // Analyze pixel colors
            let mut total_r: u64 = 0;
            let mut total_g: u64 = 0;
            let mut total_b: u64 = 0;
            let mut text_count: u32 = 0;
            let mut text_r: u64 = 0;
            let mut text_g: u64 = 0;
            let mut text_b: u64 = 0;
            let pixel_count = crop.width() * crop.height();

            for pixel in crop.pixels() {
                total_r += pixel[0] as u64;
                total_g += pixel[1] as u64;
                total_b += pixel[2] as u64;
                let brightness = (pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32) / 3;
                if brightness > 100 {
                    text_count += 1;
                    text_r += pixel[0] as u64;
                    text_g += pixel[1] as u64;
                    text_b += pixel[2] as u64;
                }
            }

            info!("Row {} {}: region ({}, {}, 280, 35)", row, col_name, x_start, text_y);
            info!("  Mean RGB: ({:.1}, {:.1}, {:.1})",
                total_r as f64 / pixel_count as f64,
                total_g as f64 / pixel_count as f64,
                total_b as f64 / pixel_count as f64);

            if text_count > 0 {
                info!("  Text pixels (brightness>100): {} / {} ({:.1}%)",
                    text_count, pixel_count, 100.0 * text_count as f64 / pixel_count as f64);
                info!("  Text mean RGB: ({:.1}, {:.1}, {:.1})",
                    text_r as f64 / text_count as f64,
                    text_g as f64 / text_count as f64,
                    text_b as f64 / text_count as f64);

                // Count pixels above each threshold
                for threshold in [130u32, 140, 150, 160] {
                    let above: u32 = crop.pixels()
                        .filter(|p| (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3 > threshold)
                        .count() as u32;
                    info!("  Pixels > {}: {} ({:.1}%)", threshold, above,
                        100.0 * above as f64 / pixel_count as f64);
                }
            } else {
                info!("  No text pixels found (all brightness <= 100)");
            }

            // Save binarized versions at different thresholds
            for threshold in [120u32, 130, 140, 150, 160] {
                let bin = binarize(&crop, threshold);
                save_image(&bin, &out_dir.join(format!("{}_bin{}.png", base, threshold)).to_string_lossy());
            }
        }
    }

    // Step 8: Return to main world
    info!("Step 7: Returning to main world...");
    ctrl.key_press(enigo::Key::Escape);
    utils::sleep(500);
    ctrl.key_press(enigo::Key::Escape);
    utils::sleep(500);
    ctrl.key_press(enigo::Key::Escape);
    utils::sleep(500);

    info!("Done! Check {}", out_dir.display());
    Ok(())
}
