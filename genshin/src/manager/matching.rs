use crate::scanner::common::models::{GoodArtifact, GoodSubStat};

/// Value tolerance for substat matching (accounts for OCR rounding).
/// Slightly above 0.1 to handle f64 representation (e.g., 4.0 - 3.9 = 0.10000000000000053).
const VALUE_TOLERANCE: f64 = 0.100001;

/// Hard-match substat lists: same keys (order-independent) and each value within tolerance.
/// Returns `false` if key sets differ or any value differs by more than `VALUE_TOLERANCE`.
fn substats_match(scanned: &[GoodSubStat], target: &[GoodSubStat]) -> bool {
    if scanned.len() != target.len() {
        return false;
    }
    if scanned.is_empty() {
        return true;
    }
    for ts in target {
        match scanned.iter().find(|s| s.key == ts.key) {
            Some(ss) => {
                if (ss.value - ts.value).abs() > VALUE_TOLERANCE {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

/// Match a scanned `GoodArtifact` against a target `GoodArtifact`.
///
/// ALL fields are hard-match: set, slot, rarity, level, main stat, substats,
/// and unactivated substats. Returns `None` if any field doesn't match.
/// Substat values allow 0.1 tolerance for OCR rounding.
///
/// When a match passes, returns `Some(1.0)` — all matches are equally confident.
///
/// 将扫描到的 `GoodArtifact` 与目标 `GoodArtifact` 进行匹配。
/// 所有字段均为硬匹配，副词条值允许 0.1 误差。不匹配则返回 None。
pub fn match_score(scanned: &GoodArtifact, target: &GoodArtifact) -> Option<f64> {
    if scanned.rarity != target.rarity {
        return None;
    }
    if scanned.slot_key != target.slot_key {
        return None;
    }
    if scanned.set_key != target.set_key {
        return None;
    }
    if scanned.level != target.level {
        return None;
    }
    if scanned.main_stat_key != target.main_stat_key {
        return None;
    }
    if !substats_match(&scanned.substats, &target.substats) {
        return None;
    }
    if !substats_match(&scanned.unactivated_substats, &target.unactivated_substats) {
        return None;
    }
    if scanned.elixir_crafted != target.elixir_crafted {
        return None;
    }
    Some(1.0)
}

/// Find the best matching instruction index for a scanned artifact.
/// Returns `(index, score)` of the best match, or None if no instruction matches.
///
/// 找到与扫描到的圣遗物最佳匹配的指令索引。
pub fn find_best_match(
    scanned: &GoodArtifact,
    targets: &[(usize, &GoodArtifact)],
) -> Option<(usize, f64)> {
    let mut best: Option<(usize, f64)> = None;
    for &(idx, target) in targets {
        if let Some(score) = match_score(scanned, target) {
            if best.map_or(true, |(_, best_score)| score > best_score) {
                best = Some((idx, score));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::common::models::{GoodArtifact, GoodSubStat};

    fn make_artifact(set: &str, slot: &str, rarity: i32, level: i32, main: &str, subs: &[(&str, f64)]) -> GoodArtifact {
        GoodArtifact {
            set_key: set.to_string(),
            slot_key: slot.to_string(),
            rarity,
            level,
            main_stat_key: main.to_string(),
            substats: subs.iter().map(|(k, v)| GoodSubStat {
                key: k.to_string(),
                value: *v,
                initial_value: None,
                rolls: vec![],
            }).collect(),
            location: String::new(),
            lock: false,
            astral_mark: false,
            elixir_crafted: false,
            unactivated_substats: Vec::new(),
            total_rolls: None,
        }
    }

    fn make_artifact_with_unactivated(
        set: &str, slot: &str, rarity: i32, level: i32, main: &str,
        subs: &[(&str, f64)], unact: &[(&str, f64)],
    ) -> GoodArtifact {
        let mut art = make_artifact(set, slot, rarity, level, main, subs);
        art.unactivated_substats = unact.iter().map(|(k, v)| GoodSubStat {
            key: k.to_string(),
            value: *v,
            initial_value: None,
            rolls: vec![],
        }).collect();
        art
    }

    #[test]
    fn test_exact_match() {
        let scanned = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8), ("def", 23.0)]);
        let target = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8), ("def", 23.0)]);
        assert!(match_score(&scanned, &target).is_some());
    }

    #[test]
    fn test_hard_field_mismatch() {
        let scanned = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp", &[]);
        assert!(match_score(&scanned, &make_artifact("GladiatorsFinale", "plume", 5, 20, "hp", &[])).is_none());
        assert!(match_score(&scanned, &make_artifact("GladiatorsFinale", "flower", 4, 20, "hp", &[])).is_none());
        assert!(match_score(&scanned, &make_artifact("GladiatorsFinale", "flower", 5, 16, "hp", &[])).is_none());
    }

    #[test]
    fn test_substat_key_mismatch_rejects() {
        let scanned = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8), ("def", 23.0)]);
        let target = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8), ("hp_", 4.7)]);
        assert!(match_score(&scanned, &target).is_none(), "Different substat keys must reject");
    }

    #[test]
    fn test_substat_value_outside_tolerance_rejects() {
        let scanned = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8)]);
        let target = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 10.5), ("critDMG_", 15.6)]);
        assert!(match_score(&scanned, &target).is_none(), "Values outside 0.1 tolerance must reject");
    }

    #[test]
    fn test_substat_value_within_tolerance_matches() {
        let scanned = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.85)]);
        let target = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8)]);
        assert!(match_score(&scanned, &target).is_some(), "Values within 0.1 tolerance must match");
    }

    #[test]
    fn test_substat_value_at_tolerance_boundary() {
        // Exactly 0.1 diff should pass
        let scanned = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 4.0)]);
        let target = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9)]);
        assert!(match_score(&scanned, &target).is_some(), "Exactly 0.1 diff must match");

        // 0.11 diff should reject
        let scanned2 = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 4.01)]);
        assert!(match_score(&scanned2, &target).is_none(), "0.11 diff must reject");
    }

    #[test]
    fn test_substat_count_mismatch_rejects() {
        let scanned = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8)]);
        let target = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8)]);
        assert!(match_score(&scanned, &target).is_none(), "Different substat count must reject");
    }

    #[test]
    fn test_unactivated_substats_hard_match() {
        let scanned = make_artifact_with_unactivated(
            "GladiatorsFinale", "flower", 5, 0, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8)],
            &[("def", 23.0)],
        );
        let target_match = make_artifact_with_unactivated(
            "GladiatorsFinale", "flower", 5, 0, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8)],
            &[("def", 23.0)],
        );
        let target_wrong_key = make_artifact_with_unactivated(
            "GladiatorsFinale", "flower", 5, 0, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8)],
            &[("hp_", 4.7)],
        );
        let target_no_unact = make_artifact(
            "GladiatorsFinale", "flower", 5, 0, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8)],
        );
        assert!(match_score(&scanned, &target_match).is_some());
        assert!(match_score(&scanned, &target_wrong_key).is_none(), "Wrong unactivated key must reject");
        assert!(match_score(&scanned, &target_no_unact).is_none(), "Missing unactivated must reject");
    }

    #[test]
    fn test_elixir_crafted_hard_match() {
        let mut scanned = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8)]);
        scanned.elixir_crafted = true;

        let mut target_elixir = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8)]);
        target_elixir.elixir_crafted = true;

        let target_normal = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8)]);

        assert!(match_score(&scanned, &target_elixir).is_some(), "Same elixir status must match");
        assert!(match_score(&scanned, &target_normal).is_none(), "Different elixir status must reject");
    }
}
