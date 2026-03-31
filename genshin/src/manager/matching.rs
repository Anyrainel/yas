use std::collections::HashSet;

use crate::scanner::common::models::{GoodArtifact, GoodSubStat};

/// Score substat list matching: keys (order-independent) + values for disambiguation.
fn score_substats(scanned: &[GoodSubStat], target: &[GoodSubStat]) -> f64 {
    if scanned.is_empty() && target.is_empty() {
        return 0.0;
    }

    let scanned_keys: HashSet<&str> = scanned.iter().map(|s| s.key.as_str()).collect();
    let target_keys: HashSet<&str> = target.iter().map(|s| s.key.as_str()).collect();

    if scanned_keys == target_keys {
        let mut score = 20.0;
        for ts in target {
            if let Some(ss) = scanned.iter().find(|s| s.key == ts.key) {
                let diff = (ss.value - ts.value).abs();
                let tolerance = if ts.key.ends_with('_') { 0.2 } else { 1.5 };
                if diff <= tolerance {
                    score += 5.0;
                }
            }
        }
        score
    } else {
        let overlap = scanned_keys.intersection(&target_keys).count();
        overlap as f64 * 3.0
    }
}

/// Match a scanned `GoodArtifact` against a target `GoodArtifact`.
///
/// Returns `None` if hard fields (set, slot, rarity, level, main stat) don't match.
/// Returns `Some(score)` where higher score means better match.
/// Substats (including unactivated) are used for disambiguation when multiple artifacts
/// share the same hard fields.
///
/// 将扫描到的 `GoodArtifact` 与目标 `GoodArtifact` 进行匹配。
/// 如果硬性字段不匹配则返回 None，否则返回匹配分数（越高越好）。
pub fn match_score(scanned: &GoodArtifact, target: &GoodArtifact) -> Option<f64> {
    // Hard-reject on any mismatch in highly reliable fields
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

    // Base score for matching all hard fields
    let mut score: f64 = 50.0;
    score += score_substats(&scanned.substats, &target.substats);
    score += score_substats(&scanned.unactivated_substats, &target.unactivated_substats);
    Some(score)
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
        }).collect();
        art
    }

    #[test]
    fn test_exact_match() {
        let scanned = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8), ("def", 23.0)]);
        let target = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8), ("def", 23.0)]);
        let score = match_score(&scanned, &target);
        assert!(score.is_some());
        assert!(score.unwrap() > 80.0);
    }

    #[test]
    fn test_hard_field_mismatch() {
        let scanned = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp", &[]);
        assert!(match_score(&scanned, &make_artifact("GladiatorsFinale", "plume", 5, 20, "hp", &[])).is_none());
        assert!(match_score(&scanned, &make_artifact("GladiatorsFinale", "flower", 4, 20, "hp", &[])).is_none());
        assert!(match_score(&scanned, &make_artifact("GladiatorsFinale", "flower", 5, 16, "hp", &[])).is_none());
    }

    #[test]
    fn test_partial_substat_match() {
        let scanned = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8), ("def", 23.0)]);
        let target = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8), ("hp_", 4.7)]);
        let score = match_score(&scanned, &target);
        assert!(score.is_some());
        assert!(score.unwrap() < 70.0);
    }

    #[test]
    fn test_disambiguation_by_value() {
        let art1 = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8)]);
        let art2 = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 10.5), ("critDMG_", 15.6)]);
        let target = make_artifact("GladiatorsFinale", "flower", 5, 20, "hp",
            &[("critRate_", 10.5), ("critDMG_", 15.6)]);
        let score1 = match_score(&art1, &target).unwrap();
        let score2 = match_score(&art2, &target).unwrap();
        assert!(score2 > score1, "Closer values should score higher");
    }

    #[test]
    fn test_unactivated_substats_matching() {
        let scanned = make_artifact_with_unactivated(
            "GladiatorsFinale", "flower", 5, 0, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8)],
            &[("def", 23.0)],
        );
        let target_with = make_artifact_with_unactivated(
            "GladiatorsFinale", "flower", 5, 0, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8)],
            &[("def", 23.0)],
        );
        let target_without = make_artifact(
            "GladiatorsFinale", "flower", 5, 0, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8)],
        );
        let score_with = match_score(&scanned, &target_with).unwrap();
        let score_without = match_score(&scanned, &target_without).unwrap();
        assert!(score_with > score_without, "Matching unactivated substats should score higher");
    }

    #[test]
    fn test_unactivated_substat_disambiguation() {
        let art1 = make_artifact_with_unactivated(
            "GladiatorsFinale", "flower", 5, 0, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8)],
            &[("def", 23.0)],
        );
        let art2 = make_artifact_with_unactivated(
            "GladiatorsFinale", "flower", 5, 0, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8)],
            &[("hp_", 4.7)],
        );
        let target = make_artifact_with_unactivated(
            "GladiatorsFinale", "flower", 5, 0, "hp",
            &[("critRate_", 3.9), ("critDMG_", 7.8), ("atk_", 5.8)],
            &[("def", 23.0)],
        );
        let score1 = match_score(&art1, &target).unwrap();
        let score2 = match_score(&art2, &target).unwrap();
        assert!(score1 > score2, "Matching unactivated substat should win disambiguation");
    }
}
