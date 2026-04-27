//! One-off evaluation: compare ppocrv4 vs ppocrv5 across all OCR domains.
//!
//! Usage: ocr_eval <scan.json> <gt.json> <dump_dir>
//!
//! Evaluates both engines on:
//! - Artifact substats (sub0..sub3.png)
//! - Artifact names, set names, main stats, levels, equip text
//! - Weapon names, levels, refinements, equip text
//! - Character names, levels, talents
//!
//! Prints per-engine accuracy by domain and field type.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use yas::ocr::ImageToText;
use genshin_scanner::scanner::common::ocr_factory::create_ocr_model;
use genshin_scanner::scanner::common::stat_parser;

// ─── Data structures ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
struct Artifact {
    #[serde(rename = "setKey")]
    set_key: String,
    #[serde(rename = "slotKey")]
    slot_key: String,
    level: i32,
    rarity: i32,
    #[serde(rename = "mainStatKey")]
    main_stat_key: String,
    substats: Vec<SubStat>,
    #[serde(default)]
    location: String,
    #[serde(default)]
    lock: bool,
    #[serde(default, rename = "unactivatedSubstats")]
    unactivated_substats: Vec<SubStat>,
}

#[derive(Debug, Deserialize, Clone)]
struct SubStat {
    key: String,
    value: f64,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
struct Character {
    key: String,
    level: i32,
    constellation: i32,
    ascension: i32,
    talent: Talent,
}

#[derive(Debug, Deserialize, Clone)]
struct Talent {
    auto: i32,
    skill: i32,
    burst: i32,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
struct Weapon {
    key: String,
    level: i32,
    #[serde(default)]
    ascension: i32,
    refinement: i32,
    #[serde(default)]
    location: String,
    #[serde(default)]
    lock: bool,
}

#[derive(Debug, Deserialize)]
struct Export {
    #[serde(default)]
    artifacts: Vec<Artifact>,
    #[serde(default)]
    characters: Vec<Character>,
    #[serde(default)]
    weapons: Vec<Weapon>,
}

// ─── Accuracy tracker ───────────────────────────────────────────────────────

#[derive(Default)]
struct FieldStats {
    total: u32,
    v4_correct: u32,
    v5_correct: u32,
    v4_only: u32,
    v5_only: u32,
    both_correct: u32,
    both_wrong: u32,
    disagreements: Vec<String>,
}

impl FieldStats {
    fn record(&mut self, label: &str, gt_val: &str, v4_text: &str, v5_text: &str,
              v4_match: bool, v5_match: bool) {
        self.total += 1;
        if v4_match { self.v4_correct += 1; }
        if v5_match { self.v5_correct += 1; }
        match (v4_match, v5_match) {
            (true, true) => self.both_correct += 1,
            (true, false) => {
                self.v4_only += 1;
                self.disagreements.push(format!(
                    "  {} GT={} | v4=「{}」(OK) | v5=「{}」(WRONG)", label, gt_val, v4_text, v5_text));
            },
            (false, true) => {
                self.v5_only += 1;
                self.disagreements.push(format!(
                    "  {} GT={} | v4=「{}」(WRONG) | v5=「{}」(OK)", label, gt_val, v4_text, v5_text));
            },
            (false, false) => {
                self.both_wrong += 1;
                self.disagreements.push(format!(
                    "  {} GT={} | v4=「{}」(WRONG) | v5=「{}」(WRONG)", label, gt_val, v4_text, v5_text));
            },
        }
    }

    fn print_summary(&self, name: &str) {
        if self.total == 0 { return; }
        let t = self.total as f64;
        println!("  {:<25} {:>5} total | v4: {:>5} ({:>6.2}%) | v5: {:>5} ({:>6.2}%) | v4-only: {:>4} | v5-only: {:>4} | both-wrong: {:>4}",
            name, self.total,
            self.v4_correct, self.v4_correct as f64 / t * 100.0,
            self.v5_correct, self.v5_correct as f64 / t * 100.0,
            self.v4_only, self.v5_only, self.both_wrong);
    }

    fn print_disagreements(&self, name: &str, max: usize) {
        if self.disagreements.is_empty() { return; }
        let non_both_correct: Vec<&String> = self.disagreements.iter().collect();
        if non_both_correct.is_empty() { return; }
        println!("\n  [{}] {} issues:", name, non_both_correct.len());
        for (i, d) in non_both_correct.iter().enumerate() {
            if i >= max { println!("    ... and {} more", non_both_correct.len() - max); break; }
            println!("{}", d);
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn ocr_image(engine: &dyn ImageToText<image::RgbImage>, path: &std::path::Path) -> String {
    if !path.exists() { return String::new(); }
    let img = match image::open(path) {
        Ok(i) => i.to_rgb8(),
        Err(_) => return String::new(),
    };
    engine.image_to_text(&img, false).unwrap_or_default().trim().to_string()
}

/// Extract a number from OCR text (for levels, talents, refinements)
fn extract_number(text: &str) -> Option<i32> {
    // Find all digit sequences and return the first meaningful one
    let mut nums: Vec<i32> = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            current.push(c);
        } else if !current.is_empty() {
            if let Ok(n) = current.parse::<i32>() {
                nums.push(n);
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        if let Ok(n) = current.parse::<i32>() {
            nums.push(n);
        }
    }
    nums.first().copied()
}

// ─── Artifact matching ──────────────────────────────────────────────────────

fn artifact_match_key(a: &Artifact) -> String {
    format!("{}|{}|{}|{}", a.set_key, a.slot_key, a.rarity, a.lock)
}

fn find_gt_artifact<'a>(
    scan: &Artifact,
    gt_lookup: &HashMap<String, Vec<&'a Artifact>>,
    used: &mut HashMap<String, Vec<bool>>,
) -> Option<&'a Artifact> {
    let mk = artifact_match_key(scan);
    let candidates = gt_lookup.get(&mk)?;
    let used_flags = used.get_mut(&mk)?;

    let scan_sub_keys: Vec<&str> = scan.substats.iter().map(|s| s.key.as_str()).collect();
    let mut best: Option<(i32, usize)> = None;

    for (i, &gt_a) in candidates.iter().enumerate() {
        if used_flags[i] { continue; }
        let mut score = 0i32;
        if gt_a.level == scan.level { score += 10; }
        if gt_a.main_stat_key == scan.main_stat_key { score += 10; }
        let gt_sub_keys: Vec<&str> = gt_a.substats.iter().map(|s| s.key.as_str()).collect();
        for sk in &scan_sub_keys {
            if gt_sub_keys.contains(sk) { score += 3; }
        }
        for ss in &scan.substats {
            for gs in &gt_a.substats {
                if ss.key == gs.key && (ss.value - gs.value).abs() < 0.01 { score += 5; }
            }
        }
        if best.is_none() || score > best.unwrap().0 { best = Some((score, i)); }
    }

    if let Some((_, idx)) = best {
        used_flags[idx] = true;
        Some(candidates[idx])
    } else {
        None
    }
}

// ─── Main ───────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Warn)
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("Usage: ocr_eval <scan.json> <gt.json> <dump_dir>");
        eprintln!("  scan.json: scanner output (items in dump order)");
        eprintln!("  gt.json:   groundtruth export");
        eprintln!("  dump_dir:  path to debug_images/");
        std::process::exit(1);
    }

    let scan_path = &args[1];
    let gt_path = &args[2];
    let dump_dir = PathBuf::from(&args[3]);

    // Load JSONs
    println!("Loading scan: {}", scan_path);
    let scan: Export = serde_json::from_str(
        &std::fs::read_to_string(scan_path).context("reading scan JSON")?
    ).context("parsing scan JSON")?;
    println!("  artifacts={} characters={} weapons={}",
        scan.artifacts.len(), scan.characters.len(), scan.weapons.len());

    println!("Loading GT: {}", gt_path);
    let gt: Export = serde_json::from_str(
        &std::fs::read_to_string(gt_path).context("reading GT JSON")?
    ).context("parsing GT JSON")?;
    println!("  artifacts={} characters={} weapons={}",
        gt.artifacts.len(), gt.characters.len(), gt.weapons.len());

    // Load OCR models
    println!("Loading OCR models...");
    let v4 = create_ocr_model("ppocrv4")?;
    let v5 = create_ocr_model("ppocrv5")?;
    println!("Models loaded.\n");

    // ── Artifact evaluation ─────────────────────────────────────────────
    let art_dir = dump_dir.join("artifacts");
    if art_dir.exists() && !scan.artifacts.is_empty() {
        println!("=== ARTIFACT EVALUATION ===");
        let mut sub_stats = FieldStats::default();
        let mut art_level = FieldStats::default();
        let mut art_name = FieldStats::default();
        let mut art_set = FieldStats::default();
        let mut art_main = FieldStats::default();
        let mut art_equip = FieldStats::default();

        // Build GT lookup
        let mut gt_lookup: HashMap<String, Vec<&Artifact>> = HashMap::new();
        for a in &gt.artifacts { gt_lookup.entry(artifact_match_key(a)).or_default().push(a); }
        let mut used: HashMap<String, Vec<bool>> = HashMap::new();
        for (k, v) in &gt_lookup { used.insert(k.clone(), vec![false; v.len()]); }

        for (idx, scan_art) in scan.artifacts.iter().enumerate() {
            let dir = art_dir.join(format!("{:04}", idx));
            if !dir.exists() { continue; }

            let gt_art = find_gt_artifact(scan_art, &gt_lookup, &mut used);

            // Substats (need GT match)
            if let Some(gt_a) = gt_art {
                let gt_subs: Vec<&SubStat> = gt_a.substats.iter()
                    .chain(gt_a.unactivated_substats.iter()).collect();

                for line in 0..4u32 {
                    let sub_path = dir.join(format!("sub{}.png", line));
                    if line as usize >= gt_subs.len() { break; }
                    let gt_sub = gt_subs[line as usize];

                    let t4 = ocr_image(v4.as_ref(), &sub_path);
                    let t5 = ocr_image(v5.as_ref(), &sub_path);
                    let p4 = stat_parser::parse_stat_from_text(&t4);
                    let p5 = stat_parser::parse_stat_from_text(&t5);

                    let v4_ok = p4.as_ref().map_or(false, |p| p.key == gt_sub.key && (p.value - gt_sub.value).abs() < 0.05);
                    let v5_ok = p5.as_ref().map_or(false, |p| p.key == gt_sub.key && (p.value - gt_sub.value).abs() < 0.05);

                    let v4_str = p4.map_or("NONE".into(), |p| format!("{}={}", p.key, p.value));
                    let v5_str = p5.map_or("NONE".into(), |p| format!("{}={}", p.key, p.value));

                    sub_stats.record(
                        &format!("[{:04}/sub{}]", idx, line),
                        &format!("{}={}", gt_sub.key, gt_sub.value),
                        &v4_str, &v5_str, v4_ok, v5_ok,
                    );
                }

                // Level
                let t4 = ocr_image(v4.as_ref(), &dir.join("level.png"));
                let t5 = ocr_image(v5.as_ref(), &dir.join("level.png"));
                let n4 = extract_number(&t4);
                let n5 = extract_number(&t5);
                art_level.record(
                    &format!("[{:04}]", idx),
                    &gt_a.level.to_string(),
                    &format!("{:?}", n4), &format!("{:?}", n5),
                    n4 == Some(gt_a.level), n5 == Some(gt_a.level),
                );

                // Equip (location) — compare raw text, check if GT location name appears
                // Since we can't reverse GOOD key → Chinese, just compare both engines' raw text
                // against each other and the scan result.
                let t4 = ocr_image(v4.as_ref(), &dir.join("equip.png"));
                let t5 = ocr_image(v5.as_ref(), &dir.join("equip.png"));
                // For equip, "correct" = produced the same location as the scan
                let scan_loc = &scan_art.location;
                // We don't have a reliable way to check equip without the mapping,
                // so just compare if both engines produce identical text
                let same = t4 == t5;
                art_equip.record(
                    &format!("[{:04}]", idx),
                    scan_loc,
                    &t4, &t5,
                    true, same, // v4 always "baseline", v5 correct if matches v4
                );
            }

            // Name, set name, main stat — these don't need GT matching,
            // just compare both engines' text (no ground truth for raw OCR text)
            let t4 = ocr_image(v4.as_ref(), &dir.join("name.png"));
            let t5 = ocr_image(v5.as_ref(), &dir.join("name.png"));
            art_name.record(&format!("[{:04}]", idx), "-", &t4, &t5, true, t4 == t5);

            let t4 = ocr_image(v4.as_ref(), &dir.join("set_name.png"));
            let t5 = ocr_image(v5.as_ref(), &dir.join("set_name.png"));
            art_set.record(&format!("[{:04}]", idx), "-", &t4, &t5, true, t4 == t5);

            let t4 = ocr_image(v4.as_ref(), &dir.join("main_stat.png"));
            let t5 = ocr_image(v5.as_ref(), &dir.join("main_stat.png"));
            art_main.record(&format!("[{:04}]", idx), "-", &t4, &t5, true, t4 == t5);

            if (idx + 1) % 500 == 0 {
                println!("  Artifacts: {}/{}...", idx + 1, scan.artifacts.len());
            }
        }

        println!("\nArtifact Results:");
        sub_stats.print_summary("substats (key+value)");
        art_level.print_summary("level");
        art_name.print_summary("name (v4==v5?)");
        art_set.print_summary("set_name (v4==v5?)");
        art_main.print_summary("main_stat (v4==v5?)");
        art_equip.print_summary("equip (v4==v5?)");

        sub_stats.print_disagreements("substats", 30);
        art_level.print_disagreements("level", 20);
        art_name.print_disagreements("name", 20);
        art_set.print_disagreements("set_name", 20);
        art_main.print_disagreements("main_stat", 20);
    }

    // ── Weapon evaluation ───────────────────────────────────────────────
    let wpn_dir = dump_dir.join("weapons");
    if wpn_dir.exists() && !scan.weapons.is_empty() {
        println!("\n=== WEAPON EVALUATION ===");
        let mut wpn_name = FieldStats::default();
        let mut wpn_level = FieldStats::default();
        let mut wpn_refine = FieldStats::default();
        let mut wpn_equip = FieldStats::default();

        // Match weapons by index (scan order = dump order)
        // GT matching: by key+location+lock (should be unique enough)
        let mut gt_by_key: HashMap<String, Vec<(usize, &Weapon)>> = HashMap::new();
        for (i, w) in gt.weapons.iter().enumerate() {
            gt_by_key.entry(format!("{}|{}", w.key, w.lock)).or_default().push((i, w));
        }
        let mut used_gt_wpn = vec![false; gt.weapons.len()];

        for (idx, scan_wpn) in scan.weapons.iter().enumerate() {
            let dir = wpn_dir.join(format!("{:04}", idx));
            if !dir.exists() { continue; }

            // Find GT weapon
            let mk = format!("{}|{}", scan_wpn.key, scan_wpn.lock);
            let gt_wpn = if let Some(candidates) = gt_by_key.get(&mk) {
                let mut best: Option<(i32, usize)> = None;
                for &(gi, gw) in candidates {
                    if used_gt_wpn[gi] { continue; }
                    let mut score = 0i32;
                    if gw.level == scan_wpn.level { score += 10; }
                    if gw.refinement == scan_wpn.refinement { score += 5; }
                    if gw.location == scan_wpn.location { score += 5; }
                    if best.is_none() || score > best.unwrap().0 { best = Some((score, gi)); }
                }
                if let Some((_, gi)) = best {
                    used_gt_wpn[gi] = true;
                    Some(&gt.weapons[gi])
                } else { None }
            } else { None };

            let gt_wpn = match gt_wpn {
                Some(w) => w,
                None => continue,
            };

            // Name — compare raw text (can't reverse GOOD key to Chinese)
            let t4 = ocr_image(v4.as_ref(), &dir.join("name.png"));
            let t5 = ocr_image(v5.as_ref(), &dir.join("name.png"));
            wpn_name.record(&format!("[{:04}]", idx), &gt_wpn.key, &t4, &t5, true, t4 == t5);

            // Level
            let t4 = ocr_image(v4.as_ref(), &dir.join("level.png"));
            let t5 = ocr_image(v5.as_ref(), &dir.join("level.png"));
            let n4 = extract_number(&t4);
            let n5 = extract_number(&t5);
            wpn_level.record(
                &format!("[{:04}]", idx), &gt_wpn.level.to_string(),
                &format!("{:?}", n4), &format!("{:?}", n5),
                n4 == Some(gt_wpn.level), n5 == Some(gt_wpn.level),
            );

            // Refinement
            let t4 = ocr_image(v4.as_ref(), &dir.join("refinement.png"));
            let t5 = ocr_image(v5.as_ref(), &dir.join("refinement.png"));
            let n4 = extract_number(&t4);
            let n5 = extract_number(&t5);
            wpn_refine.record(
                &format!("[{:04}]", idx), &gt_wpn.refinement.to_string(),
                &format!("{:?}", n4), &format!("{:?}", n5),
                n4 == Some(gt_wpn.refinement), n5 == Some(gt_wpn.refinement),
            );

            // Equip
            let t4 = ocr_image(v4.as_ref(), &dir.join("equip.png"));
            let t5 = ocr_image(v5.as_ref(), &dir.join("equip.png"));
            wpn_equip.record(&format!("[{:04}]", idx), &gt_wpn.location, &t4, &t5, true, t4 == t5);
        }

        println!("\nWeapon Results:");
        wpn_name.print_summary("name (v4==v5?)");
        wpn_level.print_summary("level");
        wpn_refine.print_summary("refinement");
        wpn_equip.print_summary("equip (v4==v5?)");

        wpn_level.print_disagreements("level", 20);
        wpn_refine.print_disagreements("refinement", 20);
        wpn_name.print_disagreements("name", 20);
    }

    // ── Character evaluation ────────────────────────────────────────────
    let char_dir = dump_dir.join("characters");
    if char_dir.exists() && !scan.characters.is_empty() {
        println!("\n=== CHARACTER EVALUATION ===");
        let mut chr_name = FieldStats::default();
        let mut chr_level = FieldStats::default();
        let mut chr_talent_auto = FieldStats::default();
        let mut chr_talent_skill = FieldStats::default();
        let mut chr_talent_burst = FieldStats::default();

        // Match characters by index (scan order = dump order)
        // GT chars matched by key
        let gt_char_map: HashMap<&str, &Character> = gt.characters.iter()
            .map(|c| (c.key.as_str(), c)).collect();

        for (idx, scan_chr) in scan.characters.iter().enumerate() {
            let dir = char_dir.join(format!("{:04}", idx));
            if !dir.exists() { continue; }

            let gt_chr = match gt_char_map.get(scan_chr.key.as_str()) {
                Some(c) => c,
                None => continue,
            };

            // Name
            let t4 = ocr_image(v4.as_ref(), &dir.join("name.png"));
            let t5 = ocr_image(v5.as_ref(), &dir.join("name.png"));
            chr_name.record(&format!("[{:04}]", idx), &gt_chr.key, &t4, &t5, true, t4 == t5);

            // Level
            let t4 = ocr_image(v4.as_ref(), &dir.join("level.png"));
            let t5 = ocr_image(v5.as_ref(), &dir.join("level.png"));
            let n4 = extract_number(&t4);
            let n5 = extract_number(&t5);
            chr_level.record(
                &format!("[{:04}]", idx), &gt_chr.level.to_string(),
                &format!("{:?}", n4), &format!("{:?}", n5),
                n4 == Some(gt_chr.level), n5 == Some(gt_chr.level),
            );

            // Talent matching: constellation C3/C5 each add +3 to one talent
            // (skill or burst). The OCR reads the displayed value (base + bonus),
            // while GT stores the base value. Accept exact or +3 match.
            let c = gt_chr.constellation;

            // Helper: check if OCR value matches GT base or GT base + 3
            let talent_match = |ocr_val: Option<i32>, gt_base: i32, may_have_bonus: bool| -> bool {
                match ocr_val {
                    Some(v) => v == gt_base || (may_have_bonus && v == gt_base + 3),
                    None => false,
                }
            };

            // Auto: only gets +3 bonus for a few characters (very rare, but allow it at C>=3)
            let t4 = ocr_image(v4.as_ref(), &dir.join("talent_auto.png"));
            let t5 = ocr_image(v5.as_ref(), &dir.join("talent_auto.png"));
            let n4 = extract_number(&t4);
            let n5 = extract_number(&t5);
            chr_talent_auto.record(
                &format!("[{:04}]", idx),
                &format!("{}{}", gt_chr.talent.auto, if c >= 3 { "(+3?)" } else { "" }),
                &format!("{:?}", n4), &format!("{:?}", n5),
                talent_match(n4, gt_chr.talent.auto, c >= 3),
                talent_match(n5, gt_chr.talent.auto, c >= 3),
            );

            // Skill: gets +3 at C3 or C5 (we don't know which, so allow at C>=3)
            let t4 = ocr_image(v4.as_ref(), &dir.join("talent_skill.png"));
            let t5 = ocr_image(v5.as_ref(), &dir.join("talent_skill.png"));
            let n4 = extract_number(&t4);
            let n5 = extract_number(&t5);
            chr_talent_skill.record(
                &format!("[{:04}]", idx),
                &format!("{}{}", gt_chr.talent.skill, if c >= 3 { "(+3?)" } else { "" }),
                &format!("{:?}", n4), &format!("{:?}", n5),
                talent_match(n4, gt_chr.talent.skill, c >= 3),
                talent_match(n5, gt_chr.talent.skill, c >= 3),
            );

            // Burst: gets +3 at C3 or C5 (we don't know which, so allow at C>=3)
            let t4 = ocr_image(v4.as_ref(), &dir.join("talent_burst.png"));
            let t5 = ocr_image(v5.as_ref(), &dir.join("talent_burst.png"));
            let n4 = extract_number(&t4);
            let n5 = extract_number(&t5);
            chr_talent_burst.record(
                &format!("[{:04}]", idx),
                &format!("{}{}", gt_chr.talent.burst, if c >= 3 { "(+3?)" } else { "" }),
                &format!("{:?}", n4), &format!("{:?}", n5),
                talent_match(n4, gt_chr.talent.burst, c >= 3),
                talent_match(n5, gt_chr.talent.burst, c >= 3),
            );
        }

        println!("\nCharacter Results:");
        chr_name.print_summary("name (v4==v5?)");
        chr_level.print_summary("level");
        chr_talent_auto.print_summary("talent_auto");
        chr_talent_skill.print_summary("talent_skill");
        chr_talent_burst.print_summary("talent_burst");

        chr_level.print_disagreements("level", 20);
        chr_talent_auto.print_disagreements("talent_auto", 20);
        chr_talent_skill.print_disagreements("talent_skill", 20);
        chr_talent_burst.print_disagreements("talent_burst", 20);
        chr_name.print_disagreements("name", 20);
    }

    println!("\nDone.");
    Ok(())
}
