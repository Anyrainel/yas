//! Quick CLI tool to OCR a given image using ppocrv4 and ppocrv5.
//!
//! Usage:
//!   ocr_test <image_path> [--crop-right <pixels>]
//!   ocr_test --equip <image_path>
//!
//! Default mode: runs both engines, prints raw text and stat parsing results.
//! --equip mode: tests the full equip pipeline (OCR → parse → fuzzy match).

use anyhow::Result;
use yas_genshin::scanner::common::ocr_factory::create_ocr_model;
use yas_genshin::scanner::common::fuzzy_match::fuzzy_match_map;
use yas_genshin::scanner::common::mappings::{MappingManager, NameOverrides};

/// Parse equipped character from equip text (mirrors scanner logic).
fn parse_equip_location(text: &str, char_map: &std::collections::HashMap<String, String>) -> String {
    let equip_marker = if text.contains("\u{5DF2}\u{88C5}\u{5907}") {
        Some("\u{5DF2}\u{88C5}\u{5907}") // 已装备
    } else if text.contains("\u{5DF2}\u{88C5}") {
        Some("\u{5DF2}\u{88C5}") // 已装 (truncated)
    } else {
        None
    };

    if let Some(marker) = equip_marker {
        let char_name = text
            .replace(marker, "")
            .replace(['\u{5907}', ':', '\u{FF1A}', ' '], "")
            .trim()
            .to_string();

        let cleaned: String = char_name
            .trim_start_matches(|c: char| c.is_ascii() || !c.is_alphanumeric())
            .to_string();

        for name in [&cleaned, &char_name] {
            if !name.is_empty() {
                if let Some(key) = fuzzy_match_map(name, char_map) {
                    return key;
                }
            }
        }

        // No match — show what we extracted
        if !char_name.is_empty() {
            return format!("(no match for {:?})", char_name);
        }
    }
    String::new()
}

fn main() -> Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: ocr_test <image_path> [--crop-right <pixels>]");
        eprintln!("       ocr_test --equip <image_path>");
        std::process::exit(1);
    }

    // Check for --equip mode
    if args[1] == "--equip" {
        if args.len() < 3 {
            eprintln!("Usage: ocr_test --equip <image_path>");
            std::process::exit(1);
        }
        return run_equip_test(&args[2]);
    }

    let image_path = &args[1];
    let mut crop_right: u32 = 0;
    if args.len() >= 4 && args[2] == "--crop-right" {
        crop_right = args[3].parse().unwrap_or(0);
    }

    // Load image
    let img = image::open(image_path)?.to_rgb8();
    println!("Image: {} ({}x{})", image_path, img.width(), img.height());

    // Optionally crop right side
    let img = if crop_right > 0 && crop_right < img.width() {
        let new_w = img.width() - crop_right;
        println!("Cropping {}px from right -> {}x{}", crop_right, new_w, img.height());
        image::imageops::crop_imm(&img, 0, 0, new_w, img.height()).to_image()
    } else {
        img
    };

    // Load models
    println!("Loading models...");
    let v4 = create_ocr_model("ppocrv4")?;
    let v5 = create_ocr_model("ppocrv5")?;

    // Run OCR
    let result_v4 = v4.image_to_text(&img, false)?;
    let result_v5 = v5.image_to_text(&img, false)?;

    println!();
    println!("ppocrv4: {:?}", result_v4);
    println!("ppocrv5: {:?}", result_v5);

    // Also try stat parsing
    let parsed_v4 = yas_genshin::scanner::common::stat_parser::parse_stat_from_text(result_v4.trim());
    let parsed_v5 = yas_genshin::scanner::common::stat_parser::parse_stat_from_text(result_v5.trim());

    println!();
    if let Some(p) = &parsed_v4 {
        println!("ppocrv4 parsed: key={}, value={}, inactive={}", p.key, p.value, p.inactive);
    } else {
        println!("ppocrv4 parsed: None");
    }
    if let Some(p) = &parsed_v5 {
        println!("ppocrv5 parsed: key={}, value={}, inactive={}", p.key, p.value, p.inactive);
    } else {
        println!("ppocrv5 parsed: None");
    }

    Ok(())
}

fn run_equip_test(image_path: &str) -> Result<()> {
    let img = image::open(image_path)?.to_rgb8();
    println!("Image: {} ({}x{})", image_path, img.width(), img.height());

    // Load mappings
    println!("Loading mappings...");
    let mappings = MappingManager::new(&NameOverrides::default())?;
    println!("  {} characters loaded", mappings.character_name_map.len());

    // Load models
    println!("Loading models...");
    let v4 = create_ocr_model("ppocrv4")?;
    let v5 = create_ocr_model("ppocrv5")?;

    // OCR with both engines
    let text_v4 = v4.image_to_text(&img, false)?;
    let text_v5 = v5.image_to_text(&img, false)?;

    println!();
    println!("=== Equip Pipeline Test ===");
    println!();

    // v4 path
    let loc_v4 = parse_equip_location(&text_v4, &mappings.character_name_map);
    println!("v4 OCR:   {:?}", text_v4);
    println!("v4 match: {:?}", if loc_v4.is_empty() { "(empty)" } else { &loc_v4 });

    // v5 path
    let loc_v5 = parse_equip_location(&text_v5, &mappings.character_name_map);
    println!();
    println!("v5 OCR:   {:?}", text_v5);
    println!("v5 match: {:?}", if loc_v5.is_empty() { "(empty)" } else { &loc_v5 });

    // Combined path (as scanner does it)
    println!();
    println!("=== Combined (v4 → v5 fallback) ===");
    let final_loc = if !loc_v4.is_empty() && !loc_v4.starts_with("(no match") {
        println!("v4 matched: {}", loc_v4);
        loc_v4
    } else if !loc_v5.is_empty() && !loc_v5.starts_with("(no match") {
        println!("v4 failed, v5 fallback matched: {}", loc_v5);
        loc_v5
    } else {
        println!("BOTH FAILED");
        println!("  v4: {:?} -> {:?}", text_v4, loc_v4);
        println!("  v5: {:?} -> {:?}", text_v5, loc_v5);
        String::new()
    };

    if !final_loc.is_empty() {
        println!("Result: {}", final_loc);
    }

    Ok(())
}
