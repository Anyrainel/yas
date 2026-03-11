/// Artifact roll constraint solver.
///
/// Validates OCR-derived substat values against Genshin Impact's artifact
/// roll mechanics. Each substat value must decompose into valid rolls from
/// the tier table (0.7x/0.8x/0.9x/1.0x of max), and total roll counts
/// must match what's possible for the artifact's rarity and level.
///
/// Also produces GOOD format fields: `initialValue` per substat and
/// `totalRolls` at the artifact level.

// ---------------------------------------------------------------------------
// Roll value tables — per-tier values from game data
// ---------------------------------------------------------------------------
// Source: Genshin Impact datamine, verified against CONTEXT.md max values.
// Max roll values from CONTEXT.md: each tier is max × {0.7, 0.8, 0.9, 1.0},
// stored internally at 2 decimal precision.
//
// Values are in "display units":
//   - Flat stats: actual HP/ATK/DEF/EM values
//   - Percent stats: percentage points (e.g., 5.83 means 5.83%)
//
// Display rounding (on the final summed value, NOT per roll):
//   - Flat stats (hp, atk, def, eleMas): rounded to integer (half-up)
//   - Percent stats (hp_, atk_, def_, enerRech_, critRate_, critDMG_):
//     rounded to 1 decimal place (half-up)

/// 5-star artifact substat roll tiers: [0.7x, 0.8x, 0.9x, 1.0x].
const ROLLS_5: &[(&str, [f64; 4])] = &[
    ("hp",        [209.13, 239.00, 268.88, 298.75]),
    ("hp_",       [4.08,   4.66,   5.25,   5.83]),
    ("atk",       [13.62,  15.56,  17.51,  19.45]),
    ("atk_",      [4.08,   4.66,   5.25,   5.83]),
    ("def",       [16.20,  18.52,  20.83,  23.15]),
    ("def_",      [5.10,   5.83,   6.56,   7.29]),
    ("eleMas",    [16.32,  18.65,  20.98,  23.31]),
    ("enerRech_", [4.53,   5.18,   5.83,   6.48]),
    ("critRate_", [2.72,   3.11,   3.50,   3.89]),
    ("critDMG_",  [5.44,   6.22,   6.99,   7.77]),
];

/// 4-star artifact substat roll tiers: [0.7x, 0.8x, 0.9x, 1.0x].
const ROLLS_4: &[(&str, [f64; 4])] = &[
    ("hp",        [167.30, 191.20, 215.10, 239.00]),
    ("hp_",       [3.26,   3.73,   4.20,   4.66]),
    ("atk",       [10.89,  12.45,  14.00,  15.56]),
    ("atk_",      [3.26,   3.73,   4.20,   4.66]),
    ("def",       [12.96,  14.82,  16.67,  18.52]),
    ("def_",      [4.08,   4.66,   5.25,   5.83]),
    ("eleMas",    [13.06,  14.92,  16.79,  18.65]),
    ("enerRech_", [3.63,   4.14,   4.66,   5.18]),
    ("critRate_", [2.18,   2.49,   2.80,   3.11]),
    ("critDMG_",  [4.35,   4.97,   5.60,   6.22]),
];

fn roll_tiers(key: &str, rarity: i32) -> Option<[f64; 4]> {
    let table = if rarity == 5 { ROLLS_5 } else { ROLLS_4 };
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn is_percent_stat(key: &str) -> bool {
    key.ends_with('_')
}

// ---------------------------------------------------------------------------
// Roll table lookup — pre-computed from game's exact f32 + banker's rounding
// ---------------------------------------------------------------------------
// Source: genshin-substat-lookup (rollTable.json), which enumerates ALL valid
// display values using the game's exact C# float32 arithmetic and
// string.Format banker's rounding.
//
// Each table entry is (display_value × 10, roll_count_bitmask) where
// bit N-1 is set if N rolls can produce that display value.

include!("roll_table.rs");

/// Look up a display value in the roll table. Returns the roll count bitmask,
/// or None if the value doesn't exist (i.e., the game never produces it).
fn roll_table_lookup(key: &str, rarity: i32, display_value: f64) -> Option<u8> {
    let table = roll_table(key, rarity)?;
    let key_val = (display_value * 10.0).round() as i32;
    match table.binary_search_by_key(&key_val, |&(k, _)| k) {
        Ok(idx) => Some(table[idx].1),
        Err(_) => None,
    }
}

/// Find all valid roll counts for a substat using the pre-computed roll table.
fn valid_roll_counts(key: &str, rarity: i32, display_value: f64, max_per_stat: i32) -> Vec<i32> {
    let mask = match roll_table_lookup(key, rarity, display_value) {
        Some(m) => m,
        None => return vec![],
    };
    (1..=8)
        .filter(|&n| n <= max_per_stat && (mask & (1u8 << (n - 1))) != 0)
        .collect()
}

// ---------------------------------------------------------------------------
// Display rounding (used only by compute_initial_value)
// ---------------------------------------------------------------------------

fn round_1dp_half_up(v: f64) -> f64 {
    (v * 10.0 + 0.5 + 1e-9).floor() / 10.0
}

fn round_int_half_up(v: f64) -> f64 {
    (v + 0.5 + 1e-9).floor()
}

fn matches_display(internal_sum: f64, display_value: f64, is_pct: bool) -> bool {
    if is_pct {
        let half_up = round_1dp_half_up(internal_sum);
        (half_up - display_value).abs() < 0.01
    } else {
        let half_up = round_int_half_up(internal_sum);
        (half_up - display_value).abs() < 0.01
    }
}

// ---------------------------------------------------------------------------
// Enumerate possible internal sums for N rolls (used by compute_initial_value)
// ---------------------------------------------------------------------------

fn enumerate_sums(tiers: &[f64; 4], count: i32) -> Vec<f64> {
    let mut results = Vec::new();
    enumerate_sums_rec(tiers, count, 0, 0.0, &mut results);
    results
}

fn enumerate_sums_rec(
    tiers: &[f64; 4],
    remaining: i32,
    min_tier: usize,
    current: f64,
    results: &mut Vec<f64>,
) {
    if remaining == 0 {
        results.push(current);
        return;
    }
    for t in min_tier..4 {
        enumerate_sums_rec(tiers, remaining - 1, t, current + tiers[t], results);
    }
}

// ---------------------------------------------------------------------------
// Constraint solving: find roll count assignment across all substats
// ---------------------------------------------------------------------------

/// Try to find a valid assignment of roll counts to substats.
///
/// Each substat has a list of valid roll counts. We need to find one per substat
/// such that the sum equals `total_rolls` and each is >= 1.
fn find_assignment(
    valid_counts: &[Vec<i32>],
    total_rolls: i32,
) -> Option<Vec<i32>> {
    let mut assignment = vec![0i32; valid_counts.len()];
    if backtrack(valid_counts, total_rolls, 0, &mut assignment) {
        Some(assignment)
    } else {
        None
    }
}

fn backtrack(
    valid_counts: &[Vec<i32>],
    remaining: i32,
    idx: usize,
    assignment: &mut [i32],
) -> bool {
    if idx == valid_counts.len() {
        return remaining == 0;
    }
    let min_remaining_others: i32 = (idx + 1..valid_counts.len())
        .map(|i| valid_counts[i].iter().copied().min().unwrap_or(1))
        .sum();
    for &count in &valid_counts[idx] {
        if count > remaining {
            continue;
        }
        let left = remaining - count;
        if left < min_remaining_others {
            continue;
        }
        assignment[idx] = count;
        if backtrack(valid_counts, left, idx + 1, assignment) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Determine initialValue
// ---------------------------------------------------------------------------
// The initial value is the display-rounded value of the first roll's tier.
// For roll_count==1, it equals the display value itself.
// For roll_count>1, we try each tier as the first roll and check if the
// remaining rolls can produce the difference. If only one tier works
// uniquely, we report it; otherwise we report None (ambiguous).

fn compute_initial_value(key: &str, rarity: i32, display_value: f64, roll_count: i32) -> Option<f64> {
    let tiers = roll_tiers(key, rarity)?;
    let is_pct = is_percent_stat(key);

    if roll_count == 1 {
        // initialValue == display value when there's exactly one roll
        for &tier_val in &tiers {
            if matches_display(tier_val, display_value, is_pct) {
                return Some(round_to_display(tier_val, is_pct));
            }
        }
        return None;
    }

    // For multi-roll: try each tier as initial, check if remaining rolls work
    let mut possible_initials: Vec<f64> = Vec::new();
    for &init_tier in &tiers {
        let remaining_count = roll_count - 1;
        let remaining_sums = enumerate_sums(&tiers, remaining_count);
        for &rest_sum in &remaining_sums {
            let total = init_tier + rest_sum;
            if matches_display(total, display_value, is_pct) {
                let init_display = round_to_display(init_tier, is_pct);
                if !possible_initials.iter().any(|&v| (v - init_display).abs() < 0.001) {
                    possible_initials.push(init_display);
                }
                break; // this init tier works, move to next
            }
        }
    }

    if possible_initials.len() == 1 {
        Some(possible_initials[0])
    } else {
        None // ambiguous or impossible
    }
}

/// Round an internal value to display format (half-up).
fn round_to_display(value: f64, is_pct: bool) -> f64 {
    if is_pct {
        round_1dp_half_up(value)
    } else {
        round_int_half_up(value)
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A single OCR candidate for a substat (from one engine).
#[derive(Debug, Clone)]
pub struct OcrCandidate {
    pub key: String,
    pub value: f64,
    /// Whether this candidate was parsed as inactive (待激活).
    pub inactive: bool,
}

/// Solver input: OCR candidates from dual engines.
#[derive(Debug, Clone)]
pub struct SolverInput {
    pub rarity: i32,
    /// Level candidates from dual OCR (1-2 values).
    pub level_candidates: Vec<i32>,
    /// Per substat line (0-4 lines), each with 0-N candidates.
    pub substat_candidates: Vec<Vec<OcrCandidate>>,
}

/// Solved substat with roll information.
#[derive(Debug, Clone)]
pub struct SolvedSubstat {
    pub key: String,
    pub value: f64,
    pub roll_count: i32,
    /// The value of the initial roll, if uniquely determinable (roll_count == 1).
    pub initial_value: Option<f64>,
    /// Whether this substat was parsed as inactive (待激活).
    pub inactive: bool,
}

/// Result of solving an artifact's substats.
#[derive(Debug, Clone)]
pub struct SolverResult {
    pub level: i32,
    pub substats: Vec<SolvedSubstat>,
    /// How many substats the artifact started with (before level-up additions).
    pub initial_substat_count: i32,
    /// Total rolls across all substats (initial + upgrades).
    pub total_rolls: i32,
}

/// Attempt to solve the artifact substats.
///
/// Tries all combinations of level candidates and substat candidates,
/// and for each, checks if the values are consistent with the roll mechanics.
///
/// Init count preference depends on level:
/// - Level 0: prefer higher init (the number of substat lines IS the init count)
/// - Level > 0: prefer lower init (init=3 is more common, better GT accuracy)
///
/// At level 0 with init < max_init, the artifact shows init_count + 1 substats
/// (the extra one is inactive/待激活). The solver accounts for this by trying to
/// select all visible substats before falling back to fewer.
pub fn solve(input: &SolverInput) -> Option<SolverResult> {
    if input.rarity < 4 || input.rarity > 5 {
        return None;
    }

    let max_level = if input.rarity == 5 { 20 } else { 16 };
    let max_init = if input.rarity == 5 { 4 } else { 3 };

    // Deduplicate level candidates
    let mut levels: Vec<i32> = input.level_candidates.clone();
    levels.sort();
    levels.dedup();

    // Generate all substat line selections (cartesian product).
    // Each line contributes either one candidate or "empty" (no substat on that line).
    let line_options: Vec<Vec<Option<&OcrCandidate>>> = input
        .substat_candidates
        .iter()
        .map(|cands| {
            let mut opts: Vec<Option<&OcrCandidate>> = vec![None]; // "no substat" option
            for c in cands {
                opts.push(Some(c));
            }
            opts
        })
        .collect();

    let num_candidate_lines = line_options.len();

    // Try solving with original levels first
    let result = solve_with_levels(&levels, input.rarity, max_level, max_init, &line_options);

    // If the solution uses fewer substats than available candidate lines AND
    // a level < 10 was involved, try level+10 fallback — common OCR error:
    // "11" misread as "1", "12" as "2", etc. Prefer the solution with more substats.
    let needs_fallback = match &result {
        None => true,
        Some(r) => r.substats.len() < num_candidate_lines && r.level < 10,
    };

    if needs_fallback {
        let fallback_levels: Vec<i32> = levels.iter()
            .filter(|&&l| l >= 0 && l < 10)
            .map(|&l| l + 10)
            .filter(|l| *l <= max_level && !levels.contains(l))
            .collect();
        if !fallback_levels.is_empty() {
            if let Some(fb) = solve_with_levels(&fallback_levels, input.rarity, max_level, max_init, &line_options) {
                // Prefer the fallback if it uses more substats, or if original failed
                match &result {
                    None => return Some(fb),
                    Some(r) if fb.substats.len() > r.substats.len() => return Some(fb),
                    _ => {}
                }
            }
        }
    }

    result
}

/// Inner solving loop: try each level candidate and find a valid substat assignment.
fn solve_with_levels(
    levels: &[i32],
    rarity: i32,
    max_level: i32,
    max_init: i32,
    line_options: &[Vec<Option<&OcrCandidate>>],
) -> Option<SolverResult> {
    for &level in levels {
        let level = level.clamp(0, max_level);
        let upgrades = level / 4;

        // At level 0, #lines = init count, so prefer higher init first.
        // At level > 0, prefer lower init (better GT accuracy).
        let possible_initials: &[i32] = if rarity == 5 {
            if level == 0 { &[4, 3] } else { &[3, 4] }
        } else {
            if level == 0 { &[3, 2] } else { &[2, 3] }
        };

        for &init_count in possible_initials {
            let total_rolls = init_count + upgrades;
            let adds = (4 - init_count).max(0).min(upgrades);
            let base_expected = init_count + adds;

            // At lv0 with lower init, an inactive substat is also visible.
            // Try selecting all substats (init+1) first, then fall back to init.
            let pending_inactive = level == 0 && init_count < max_init;
            let expected_variants: Vec<(i32, i32)> = if pending_inactive {
                // (expected_selected, solve_total_rolls)
                vec![(base_expected + 1, total_rolls + 1), (base_expected, total_rolls)]
            } else {
                vec![(base_expected, total_rolls)]
            };

            for &(expected_active, solve_total) in &expected_variants {
                // Try all substat line selections
                let mut selection = vec![0usize; line_options.len()];
                loop {
                    // Build current substat set
                    let chosen: Vec<&OcrCandidate> = line_options
                        .iter()
                        .zip(selection.iter())
                        .filter_map(|(opts, &idx)| opts[idx])
                        .collect();

                    let num_active = chosen.len() as i32;

                    // Check: num_active should equal expected_active, and all keys unique
                    // (an artifact cannot have two substats with the same key)
                    let has_dup_keys = {
                        let mut seen = std::collections::HashSet::new();
                        chosen.iter().any(|c| !seen.insert(c.key.as_str()))
                    };
                    if num_active == expected_active && num_active >= 1 && !has_dup_keys {
                        // Compute max rolls per stat
                        let max_per = solve_total - (num_active - 1); // at least 1 per other stat

                        let valid_counts: Vec<Vec<i32>> = chosen
                            .iter()
                            .map(|c| valid_roll_counts(&c.key, rarity, c.value, max_per))
                            .collect();

                        // If any substat has no valid roll counts, skip
                        if valid_counts.iter().all(|v| !v.is_empty()) {
                            if let Some(assignment) = find_assignment(&valid_counts, solve_total) {
                                // Build result
                                let substats: Vec<SolvedSubstat> = chosen
                                    .iter()
                                    .zip(assignment.iter())
                                    .map(|(c, &rolls)| {
                                        let init_val = compute_initial_value(
                                            &c.key, rarity, c.value, rolls,
                                        );
                                        SolvedSubstat {
                                            key: c.key.clone(),
                                            value: c.value,
                                            roll_count: rolls,
                                            initial_value: init_val,
                                            inactive: c.inactive,
                                        }
                                    })
                                    .collect();

                                // At lv0, adjust init_count based on actual inactive substats.
                                // This correctly handles the case where init=4 matches but the
                                // artifact actually has init=3 (one substat is inactive).
                                let result_init = if level == 0 {
                                    let inactive_count = substats.iter()
                                        .filter(|s| s.inactive).count() as i32;
                                    (num_active - inactive_count).max(1)
                                } else {
                                    init_count
                                };

                                return Some(SolverResult {
                                    level,
                                    substats,
                                    initial_substat_count: result_init,
                                    total_rolls: result_init + upgrades,
                                });
                            }
                        }
                    }

                    // Advance selection (odometer-style)
                    if !advance_selection(&mut selection, &line_options) {
                        break;
                    }
                }
            }
        }
    }

    None
}

/// Advance the selection indices (rightmost first). Returns false when exhausted.
fn advance_selection(selection: &mut [usize], options: &[Vec<Option<&OcrCandidate>>]) -> bool {
    for i in (0..selection.len()).rev() {
        selection[i] += 1;
        if selection[i] < options[i].len() {
            return true;
        }
        selection[i] = 0;
    }
    false
}

// ---------------------------------------------------------------------------
// Convenience: validate a single already-parsed artifact
// ---------------------------------------------------------------------------

/// Quick validation of a fully parsed artifact's substats.
///
/// Returns `true` if the substats are consistent with roll mechanics.
pub fn validate_substats(
    rarity: i32,
    level: i32,
    substats: &[(&str, f64)],
) -> bool {
    if rarity < 4 || rarity > 5 || substats.is_empty() {
        return false;
    }
    let input = SolverInput {
        rarity,
        level_candidates: vec![level],
        substat_candidates: substats
            .iter()
            .map(|(k, v)| vec![OcrCandidate { key: k.to_string(), value: *v, inactive: false }])
            .collect(),
    };
    solve(&input).is_some()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roll_tiers_exist() {
        assert!(roll_tiers("hp", 5).is_some());
        assert!(roll_tiers("critDMG_", 4).is_some());
        assert!(roll_tiers("nonexistent", 5).is_none());
    }

    #[test]
    fn test_roll_table_lookup() {
        // 5★ HP% single roll tier 4 → 5.83% → displays as 5.8 → in table
        let mask = roll_table_lookup("hp_", 5, 5.8);
        assert!(mask.is_some());
        assert!(mask.unwrap() & 0b00000001 != 0); // 1 roll valid

        // 5★ ATK flat single roll tier 4 → 19.45 → displays as 19 → in table
        assert!(roll_table_lookup("atk", 5, 19.0).is_some());

        // 5★ HP% two rolls: 11.7 → in table with 2 rolls valid
        let mask = roll_table_lookup("hp_", 5, 11.7).unwrap();
        assert!(mask & 0b00000010 != 0); // 2 rolls valid

        // 5★ Crit Rate two rolls: 7.8 → in table with 2 rolls valid
        let mask = roll_table_lookup("critRate_", 5, 7.8).unwrap();
        assert!(mask & 0b00000010 != 0);
    }

    #[test]
    fn test_impossible_value() {
        // 5★ HP% = 99.9 is impossible — not in roll table
        assert!(roll_table_lookup("hp_", 5, 99.9).is_none());
    }

    #[test]
    fn test_validate_simple_artifact() {
        // 5★ level 20 with 4 substats (all single-roll values = level 0 artifact)
        // Actually at level 20, total_rolls = 4 + 5 = 9 or 3 + 5 = 8.
        // Let's test a level 0 artifact with 3 substats (init=3, total=3).
        assert!(validate_substats(5, 0, &[
            ("critRate_", 3.9),   // tier 4: 3.89 → 3.9
            ("critDMG_", 7.8),    // tier 4: 7.77 → 7.8
            ("atk_", 5.8),        // tier 4: 5.83 → 5.8
        ]));
    }

    #[test]
    fn test_solve_returns_result() {
        let input = SolverInput {
            rarity: 5,
            level_candidates: vec![0],
            substat_candidates: vec![
                vec![OcrCandidate { key: "critRate_".into(), value: 3.9, inactive: false }],
                vec![OcrCandidate { key: "critDMG_".into(), value: 7.8, inactive: false }],
                vec![OcrCandidate { key: "atk_".into(), value: 5.8, inactive: false }],
            ],
        };
        let result = solve(&input);
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.total_rolls, 3);
        assert_eq!(r.initial_substat_count, 3);
        assert_eq!(r.substats.len(), 3);
        // Each should have roll_count = 1
        for s in &r.substats {
            assert_eq!(s.roll_count, 1);
            assert!(s.initial_value.is_some());
        }
    }

    #[test]
    fn test_solve_dual_candidates() {
        // Two OCR engines disagree on value: one says 5.8, other says 58.0
        // Only 5.8 is valid for atk_ at rarity 5
        let input = SolverInput {
            rarity: 5,
            level_candidates: vec![0],
            substat_candidates: vec![
                vec![OcrCandidate { key: "critRate_".into(), value: 3.9, inactive: false }],
                vec![OcrCandidate { key: "critDMG_".into(), value: 7.8, inactive: false }],
                vec![
                    OcrCandidate { key: "atk_".into(), value: 58.0, inactive: false },
                    OcrCandidate { key: "atk_".into(), value: 5.8, inactive: false },
                ],
            ],
        };
        let result = solve(&input);
        assert!(result.is_some());
        let r = result.unwrap();
        // Should pick the 5.8 candidate
        assert!((r.substats[2].value - 5.8).abs() < 0.01);
    }

    #[test]
    fn test_4star_validation() {
        // 4★ level 16 with 3 initial substats: total = 3 + 4 = 7 rolls, 4 active stats
        assert!(validate_substats(4, 16, &[
            ("hp_", 4.7),         // 4.66 → 4.7 (1 roll)
            ("atk", 16.0),        // 15.56 → 16 (1 roll)
            ("def_", 5.8),        // 5.83 → 5.8 (1 roll)
            ("critRate_", 12.4),  // 3.11*4 = 12.44 → 12.4 (4 rolls)
        ]));
    }

    #[test]
    fn test_real_groundtruth_artifacts() {
        // Real artifacts from genshin_export.json groundtruth data
        // 5★ lv20: def=23, atk_=9.3, hp=239, enerRech_=22.0
        assert!(validate_substats(5, 20, &[
            ("def", 23.0),
            ("atk_", 9.3),
            ("hp", 239.0),
            ("enerRech_", 22.0),
        ]));

        // 5★ lv20: critRate_=17.5, critDMG_=14.0, hp_=5.3, enerRech_=4.5
        assert!(validate_substats(5, 20, &[
            ("critRate_", 17.5),
            ("critDMG_", 14.0),
            ("hp_", 5.3),
            ("enerRech_", 4.5),
        ]));

        // 5★ lv20: critDMG_=7.0, critRate_=9.7, def=21, eleMas=79
        assert!(validate_substats(5, 20, &[
            ("critDMG_", 7.0),
            ("critRate_", 9.7),
            ("def", 21.0),
            ("eleMas", 79.0),
        ]));

        // 5★ lv20: hp=807, hp_=4.7, def=23, enerRech_=20.7
        assert!(validate_substats(5, 20, &[
            ("hp", 807.0),
            ("hp_", 4.7),
            ("def", 23.0),
            ("enerRech_", 20.7),
        ]));
    }

    #[test]
    fn test_lv0_with_inactive_substat() {
        // 5★ lv0 init=3: 3 active + 1 inactive (待激活)
        // Artifact 0909: atk_=4.7, atk=19, def=16, enerRech_=6.5 (inactive)
        let input = SolverInput {
            rarity: 5,
            level_candidates: vec![0],
            substat_candidates: vec![
                vec![OcrCandidate { key: "atk_".into(), value: 4.7, inactive: false }],
                vec![OcrCandidate { key: "atk".into(), value: 19.0, inactive: false }],
                vec![OcrCandidate { key: "def".into(), value: 16.0, inactive: false }],
                vec![OcrCandidate { key: "enerRech_".into(), value: 6.5, inactive: true }],
            ],
        };
        let result = solve(&input);
        assert!(result.is_some(), "solver should find all 4 substats");
        let r = result.unwrap();
        // Should select all 4 substats
        assert_eq!(r.substats.len(), 4, "should have 4 substats (3 active + 1 inactive)");
        // init should be 3 (not 4) because 1 substat is inactive
        assert_eq!(r.initial_substat_count, 3, "init should be 3 for lv0 with 1 inactive");
        // total_rolls should be 3 (inactive doesn't count toward active rolls)
        assert_eq!(r.total_rolls, 3, "total_rolls should exclude inactive at lv0");
        // Verify each has 1 roll
        for s in &r.substats {
            assert_eq!(s.roll_count, 1);
        }
        // Verify one is inactive
        assert_eq!(r.substats.iter().filter(|s| s.inactive).count(), 1);
    }

    #[test]
    fn test_lv0_init4_all_active() {
        // 5★ lv0 init=4: 4 active substats, no inactive
        let input = SolverInput {
            rarity: 5,
            level_candidates: vec![0],
            substat_candidates: vec![
                vec![OcrCandidate { key: "critRate_".into(), value: 3.9, inactive: false }],
                vec![OcrCandidate { key: "critDMG_".into(), value: 7.8, inactive: false }],
                vec![OcrCandidate { key: "atk_".into(), value: 5.8, inactive: false }],
                vec![OcrCandidate { key: "hp_".into(), value: 5.8, inactive: false }],
            ],
        };
        let result = solve(&input);
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.substats.len(), 4);
        assert_eq!(r.initial_substat_count, 4, "init should be 4 for all-active lv0");
        assert_eq!(r.total_rolls, 4);
    }

    #[test]
    fn test_lv0_inactive_ocr_dropped_line() {
        // 5★ lv0 init=3: OCR dropped 1 active line, only 2 active + 1 inactive
        // The solver should still find a solution with 3 substats
        let input = SolverInput {
            rarity: 5,
            level_candidates: vec![0],
            substat_candidates: vec![
                vec![OcrCandidate { key: "atk_".into(), value: 4.7, inactive: false }],
                // atk=19 line dropped (empty)
                vec![OcrCandidate { key: "def".into(), value: 16.0, inactive: false }],
                vec![OcrCandidate { key: "enerRech_".into(), value: 6.5, inactive: true }],
            ],
        };
        let result = solve(&input);
        assert!(result.is_some(), "should find solution with 3 substats");
        let r = result.unwrap();
        assert_eq!(r.substats.len(), 3);
        // With 2 active + 1 inactive, init = 2
        assert_eq!(r.initial_substat_count, 2);
        assert_eq!(r.total_rolls, 2);
    }

    #[test]
    fn test_level_plus10_fallback_more_substats() {
        // Real case: artifact #0825, 5★ lv11.
        // OCR reads level as 1. At lv1 (total=3), solver picks 3 single-roll
        // substats and drops critDMG_=13.2 (needs 2 rolls).
        // At lv11 (total=5), all 4 substats fit: 1+2+1+1=5.
        // The solver should prefer lv11 because it uses more substats.
        let input = SolverInput {
            rarity: 5,
            level_candidates: vec![1], // OCR misread "11" as "1"
            substat_candidates: vec![
                vec![OcrCandidate { key: "critRate_".into(), value: 3.1, inactive: false }], // 1 roll
                vec![OcrCandidate { key: "critDMG_".into(), value: 13.2, inactive: false }], // 2 rolls
                vec![OcrCandidate { key: "atk_".into(), value: 4.1, inactive: false }],      // 1 roll
                vec![OcrCandidate { key: "def".into(), value: 23.0, inactive: false }],      // 1 roll
            ],
        };
        let result = solve(&input);
        assert!(result.is_some(), "should solve");
        let r = result.unwrap();
        assert_eq!(r.level, 11, "should prefer lv11 (4 substats) over lv1 (3 substats)");
        assert_eq!(r.substats.len(), 4);
    }
}
