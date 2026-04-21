#!/usr/bin/env python3
"""Generate a markdown diff report with embedded OCR region images.

Usage:
    python diff_report.py [actual.json] [expected.json] [--images debug_images] [--no-images]

If no args given, auto-discovers the latest good_export_*.json as actual
and genshin_export.json as expected.
"""

import json
import sys
import os
import glob
from pathlib import Path
from collections import defaultdict
import numpy as np
from scipy.optimize import linear_sum_assignment

# Field → dump image file(s) mapping
ARTIFACT_FIELD_IMAGES = {
    "setKey": ["set_name.png"],
    "mainStatKey": ["main_stat.png"],
    "slotKey": ["slot.png"],
    "level": ["level.png"],
    "rarity": ["rarity.png"],
    "location": ["equip.png"],
    "lock": ["annotated.png"],
    "astralMark": ["annotated.png"],
    "elixirCrafted": ["elixir.png"],
}

WEAPON_FIELD_IMAGES = {
    "key": ["name.png"],
    "level": ["level.png"],
    "refinement": ["refinement.png"],
    "location": ["equip.png"],
    "lock": ["annotated.png"],
    "rarity": ["rarity.png"],
}

# For substats, any substat field maps to all sub images
SUBSTAT_IMAGES = ["sub[0].png", "sub[1].png", "sub[2].png", "sub[3].png"]


def load_json(path):
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def find_latest_export():
    """Find the most recent good_export_*.json file."""
    files = glob.glob("good_export_*.json")
    if not files:
        return None
    return max(files, key=os.path.getmtime)


def compare_substats(expected_subs, actual_subs):
    """Compare two substat lists by index, return list of (field, expected, actual) diffs."""
    diffs = []
    exp = expected_subs or []
    act = actual_subs or []

    if len(exp) != len(act):
        diffs.append(("substats.count", str(len(exp)), str(len(act))))

    for i in range(max(len(exp), len(act))):
        e = exp[i] if i < len(exp) else None
        a = act[i] if i < len(act) else None
        if e is None:
            diffs.append((f"substats[{i}]", "missing", f"{a['key']}={a['value']}"))
        elif a is None:
            diffs.append((f"substats[{i}]", f"{e['key']}={e['value']}", "missing"))
        elif e["key"] != a["key"]:
            diffs.append((f"substats[{i}].key", e["key"], a["key"]))
        elif abs(e["value"] - a["value"]) > 0.1 + 1e-6:
            diffs.append((f"substats[{i}].value({e['key']})", str(e["value"]), str(a["value"])))
    return diffs


def _diff_score(diffs):
    """Weighted score for a diff list — higher means worse match.

    Weights prioritize structural similarity:
    - slotKey: 5000 (slot is fundamental to artifact identity)
    - setKey/mainStatKey: 1000 (completely different artifact)
    - substats.count / unactivatedSubstats.count: 200 (structural shape)
    - level/rarity: 100 (major metadata mismatch)
    - missing/extra substat keys: 20 (structural mismatch)
    - substat value errors: 1 (OCR accuracy error)
    """
    score = 0
    for f, ev, av in diffs:
        if f == "slotKey":
            score += 5000
        elif f in ("setKey", "mainStatKey"):
            score += 1000
        elif f.endswith(".count"):
            score += 200
        elif f in ("level", "rarity"):
            score += 100
        elif ev in ("missing", "(missing)") or av in ("missing", "(missing)") \
                or str(ev) == "present" or str(av) == "present":
            score += 20  # key presence/absence is structural
        else:
            score += 1  # value accuracy error
    return score


def _match_within_group(exp_list, act_list):
    """Optimal matching within a group using the Hungarian algorithm.

    Finds the globally minimum-weight bipartite matching, which prevents
    the pair-stealing problem of greedy approaches.

    Returns (matched_pairs, unmatched_exp, unmatched_act).
    matched_pairs: list of (exp_idx, act_idx, exp_obj, act_obj, diffs)
    """
    if not exp_list or not act_list:
        return [], list(exp_list), list(act_list)

    n_exp = len(exp_list)
    n_act = len(act_list)

    # Build cost matrix and diff cache
    cost = np.full((n_exp, n_act), 1e9)
    diff_cache = {}
    for ei_pos, (ei, exp) in enumerate(exp_list):
        for ai_pos, (ai, act) in enumerate(act_list):
            diffs = diff_single_artifact(exp, act)
            score = _diff_score(diffs)
            cost[ei_pos, ai_pos] = score
            diff_cache[(ei_pos, ai_pos)] = (ei, ai, exp, act, diffs)

    # Hungarian algorithm — globally optimal assignment
    row_ind, col_ind = linear_sum_assignment(cost)

    matched = []
    used_exp = set()
    used_act = set()
    for r, c in zip(row_ind, col_ind):
        if cost[r, c] < 1e9:
            ei, ai, exp, act, diffs = diff_cache[(r, c)]
            matched.append((ei, ai, exp, act, diffs))
            used_exp.add(r)
            used_act.add(c)

    unmatched_exp = [(ei, exp) for pos, (ei, exp) in enumerate(exp_list) if pos not in used_exp]
    unmatched_act = [(ai, act) for pos, (ai, act) in enumerate(act_list) if pos not in used_act]
    return matched, unmatched_exp, unmatched_act


def _count_identity_diffs(diffs):
    """Count substat key diffs + setKey diff. If >=3, artifacts are different."""
    count = 0
    for f, ev, av in diffs:
        if f == "setKey":
            count += 1
        elif f.endswith(".key"):
            # substats[i].key or unactivatedSubstats[i].key
            count += 1
        elif ev == "missing" or av == "missing":
            # substats[i] missing/extra entirely
            if "substats" in f or "unactivated" in f:
                count += 1
    return count


def diff_artifacts(expected, actual):
    """Match and diff artifacts using two-phase approach.

    Phase 1: Within each (setKey, slotKey) group, find optimal pairings
             using greedy best-pair matching (not just exact matches).
    Phase 2: Pair remaining cross-group items by fewest weighted diffs.
    """
    results = []

    # Group by (setKey, slotKey, rarity, lock) — rarity and lock are pixel-based
    # and highly reliable, so they serve as hard matching requirements.
    exp_by_key = defaultdict(list)
    for i, a in enumerate(expected):
        key = (a.get("setKey", ""), a.get("slotKey", ""), a.get("rarity", 0), a.get("lock", False))
        exp_by_key[key].append((i, a))
    act_by_key = defaultdict(list)
    for i, a in enumerate(actual):
        key = (a.get("setKey", ""), a.get("slotKey", ""), a.get("rarity", 0), a.get("lock", False))
        act_by_key[key].append((i, a))

    all_keys = sorted(set(list(exp_by_key.keys()) + list(act_by_key.keys())))
    cross_unmatched_exp = []
    cross_unmatched_act = []

    # Phase 1: optimal matching within each group
    for key in all_keys:
        exp_list = list(exp_by_key.get(key, []))
        act_list = list(act_by_key.get(key, []))
        matched, um_exp, um_act = _match_within_group(exp_list, act_list)

        for ei, ai, exp, act, diffs in matched:
            if _count_identity_diffs(diffs) >= 3:
                # Too many substat key mismatches — treat as unmatched
                cross_unmatched_exp.append((ei, exp))
                cross_unmatched_act.append((ai, act))
            elif diffs:
                results.append((ai, exp.get("setKey", "?"), exp.get("slotKey", "?"), diffs, exp, act))

        cross_unmatched_exp.extend(um_exp)
        cross_unmatched_act.extend(um_act)

    # Phase 2: pair remaining cross-group items by fewest diffs
    # Only accept matches with < 2 substat key diffs
    remaining_act = list(range(len(cross_unmatched_act)))
    still_unmatched_exp = []
    for ei, exp in cross_unmatched_exp:
        best_pos = None
        best_diffs = None
        best_score = float("inf")
        for pos_idx, pos in enumerate(remaining_act):
            ai, act = cross_unmatched_act[pos]
            diffs = diff_single_artifact(exp, act)
            if _count_identity_diffs(diffs) >= 3:
                continue
            score = _diff_score(diffs)
            if score < best_score:
                best_score = score
                best_diffs = diffs
                best_pos = pos_idx
        if best_pos is not None:
            ai, act = cross_unmatched_act[remaining_act[best_pos]]
            remaining_act.pop(best_pos)
            if best_diffs:
                results.append((ai, exp.get("setKey", "?"), exp.get("slotKey", "?"), best_diffs, exp, act))
        else:
            still_unmatched_exp.append((ei, exp))

    for ei, exp in still_unmatched_exp:
        results.append((ei, exp.get("setKey", "?"), exp.get("slotKey", "?"),
                      [("_status", "MISSING from actual", "")], exp, {}))

    for pos in remaining_act:
        ai, act = cross_unmatched_act[pos]
        results.append((ai, act.get("setKey", "?"), act.get("slotKey", "?"),
                      [("_status", "", "EXTRA in actual")], {}, act))

    return results


def diff_single_artifact(exp, act):
    """Compare two artifact objects, return list of (field, expected, actual)."""
    diffs = []
    for field in ["setKey", "slotKey", "mainStatKey", "level", "rarity", "location", "lock",
                   "astralMark"]:
        ev = exp.get(field)
        av = act.get(field)
        if ev is None:
            continue
        if ev != av:
            diffs.append((field, str(ev), str(av)))

    # Handle elixirCrafted field — GT uses typo "elixerCrafted", scan uses "elixirCrafted"
    exp_elixir = exp.get("elixirCrafted", exp.get("elixerCrafted"))
    act_elixir = act.get("elixirCrafted", act.get("elixerCrafted"))
    if exp_elixir is not None and exp_elixir != act_elixir:
        diffs.append(("elixirCrafted", str(exp_elixir), str(act_elixir)))

    diffs.extend(compare_substats(exp.get("substats"), act.get("substats")))
    diffs.extend(compare_substats_named(
        "unactivatedSubstats", exp.get("unactivatedSubstats"), act.get("unactivatedSubstats")))
    return diffs


def compare_substats_named(prefix, expected_subs, actual_subs):
    """Like compare_substats but with a custom prefix, index-based."""
    diffs = []
    exp = expected_subs or []
    act = actual_subs or []

    if len(exp) != len(act):
        diffs.append((f"{prefix}.count", str(len(exp)), str(len(act))))

    for i in range(max(len(exp), len(act))):
        e = exp[i] if i < len(exp) else None
        a = act[i] if i < len(act) else None
        if e is None:
            diffs.append((f"{prefix}[{i}]", "missing", f"{a['key']}={a['value']}"))
        elif a is None:
            diffs.append((f"{prefix}[{i}]", f"{e['key']}={e['value']}", "missing"))
        elif e["key"] != a["key"]:
            diffs.append((f"{prefix}[{i}].key", e["key"], a["key"]))
        elif abs(e["value"] - a["value"]) > 0.1 + 1e-6:
            diffs.append((f"{prefix}[{i}].value({e['key']})", str(e["value"]), str(a["value"])))
    return diffs


def _weapon_diff_score(diffs):
    """Weighted score for weapon diffs — higher means worse match.

    Weights:
    - key: 1000 (completely different weapon)
    - level/ascension/rarity: 100 (major metadata)
    - refinement/location/lock: 10 (minor metadata)
    """
    score = 0
    for f, ev, av in diffs:
        if f == "key":
            score += 1000
        elif f in ("level", "ascension", "rarity"):
            score += 100
        else:
            score += 10
    return score


def _match_weapons_within_group(exp_list, act_list):
    """Optimal matching within a weapon group using the Hungarian algorithm.

    Returns (matched_pairs, unmatched_exp, unmatched_act).
    """
    if not exp_list or not act_list:
        return [], list(exp_list), list(act_list)

    n_exp = len(exp_list)
    n_act = len(act_list)

    cost = np.full((n_exp, n_act), 1e9)
    diff_cache = {}
    for ei_pos, (ei, exp) in enumerate(exp_list):
        for ai_pos, (ai, act) in enumerate(act_list):
            diffs = diff_single_weapon(exp, act)
            score = _weapon_diff_score(diffs)
            cost[ei_pos, ai_pos] = score
            diff_cache[(ei_pos, ai_pos)] = (ei, ai, exp, act, diffs)

    row_ind, col_ind = linear_sum_assignment(cost)

    matched = []
    used_exp = set()
    used_act = set()
    for r, c in zip(row_ind, col_ind):
        if cost[r, c] < 1e9:
            ei, ai, exp, act, diffs = diff_cache[(r, c)]
            matched.append((ei, ai, exp, act, diffs))
            used_exp.add(r)
            used_act.add(c)

    unmatched_exp = [(ei, exp) for pos, (ei, exp) in enumerate(exp_list) if pos not in used_exp]
    unmatched_act = [(ai, act) for pos, (ai, act) in enumerate(act_list) if pos not in used_act]
    return matched, unmatched_exp, unmatched_act


def diff_weapons(expected, actual):
    """Match and diff weapons using two-phase approach with Hungarian matching.

    Phase 1: Within each (key, rarity) group, find optimal pairings.
    Phase 2: Pair remaining cross-group items by fewest weighted diffs (Hungarian).
    """
    results = []

    # Group by (key, rarity) — rarity is pixel-based and highly reliable
    exp_by_key = defaultdict(list)
    for i, w in enumerate(expected):
        key = (w.get("key", ""), w.get("rarity", 0))
        exp_by_key[key].append((i, w))
    act_by_key = defaultdict(list)
    for i, w in enumerate(actual):
        key = (w.get("key", ""), w.get("rarity", 0))
        act_by_key[key].append((i, w))

    all_keys = sorted(set(list(exp_by_key.keys()) + list(act_by_key.keys())))
    cross_unmatched_exp = []
    cross_unmatched_act = []

    # Phase 1: optimal matching within each group
    for key in all_keys:
        exp_list = list(exp_by_key.get(key, []))
        act_list = list(act_by_key.get(key, []))
        matched, um_exp, um_act = _match_weapons_within_group(exp_list, act_list)

        for ei, ai, exp, act, diffs in matched:
            if diffs:
                results.append((ai, exp.get("key", "?"), diffs))

        cross_unmatched_exp.extend(um_exp)
        cross_unmatched_act.extend(um_act)

    # Phase 2: pair remaining cross-group items (Hungarian)
    if cross_unmatched_exp and cross_unmatched_act:
        matched, um_exp, um_act = _match_weapons_within_group(
            cross_unmatched_exp, cross_unmatched_act)
        for ei, ai, exp, act, diffs in matched:
            if diffs:
                results.append((ai, exp.get("key", "?"), diffs))
        cross_unmatched_exp = um_exp
        cross_unmatched_act = um_act

    for ei, exp in cross_unmatched_exp:
        results.append((ei, exp.get("key", "?"), [("_status", "MISSING from actual", "")]))

    for ai, act in cross_unmatched_act:
        results.append((ai, act.get("key", "?"), [("_status", "", "EXTRA in actual")]))

    return results


def diff_single_weapon(exp, act):
    diffs = []
    for field in ["key", "level", "ascension", "refinement", "rarity", "location", "lock"]:
        ev = exp.get(field)
        av = act.get(field)
        # Skip fields missing from expected (groundtruth doesn't track them)
        if ev is None:
            continue
        if ev != av:
            diffs.append((field, str(ev), str(av)))
    return diffs


def load_index_map(images_dir, category):
    """Load the index map (output position → folder index) if available."""
    map_path = os.path.join(images_dir, category, "index_map.json")
    if os.path.exists(map_path):
        with open(map_path, "r") as f:
            return json.load(f)
    return None


def find_dump_folder(images_dir, category, index, index_map=None, name_hint=""):
    """Find the dump folder for an item by output array index.

    If index_map is available, translates output position to folder name.
    """
    folder_idx = index_map[index] if index_map and index < len(index_map) else index
    exact = os.path.join(images_dir, category, f"{folder_idx:04d}")
    if os.path.isdir(exact):
        return exact
    # Fallback: try with name suffix (old format)
    pattern = os.path.join(images_dir, category, f"{folder_idx:04d}_*")
    matches = glob.glob(pattern)
    if matches:
        return matches[0]
    return None


def image_ref(folder, filename):
    """Return markdown image reference if file exists."""
    if folder is None:
        return ""
    path = os.path.join(folder, filename)
    if os.path.exists(path):
        # Use relative path for markdown
        rel = os.path.relpath(path).replace("\\", "/")
        return f"![{filename}]({rel})"
    return ""


def images_for_field(folder, field, category="artifact", act_art=None):
    """Get relevant image references for a diff field."""
    if folder is None:
        return []

    field_map = ARTIFACT_FIELD_IMAGES if category == "artifact" else WEAPON_FIELD_IMAGES
    refs = []

    if field in field_map:
        for img in field_map[field]:
            ref = image_ref(folder, img)
            if ref:
                refs.append(ref)
    elif "substats" in field or "unactivated" in field:
        # Extract index from field name like "substats[2].key" or "unactivatedSubstats[0]"
        import re
        m = re.search(r'\[(\d+)\]', field)
        if m:
            line_idx = int(m.group(1))
            # unactivatedSubstats indices continue after substats
            if "unactivated" in field and act_art:
                line_idx += len(act_art.get("substats") or [])
            if line_idx < 4:
                ref = image_ref(folder, f"sub[{line_idx}].png")
                if ref:
                    refs.append(ref)
            else:
                for img in SUBSTAT_IMAGES:
                    ref = image_ref(folder, img)
                    if ref:
                        refs.append(ref)
        else:
            for img in SUBSTAT_IMAGES:
                ref = image_ref(folder, img)
                if ref:
                    refs.append(ref)

    return refs


def diff_characters(expected, actual):
    """Match and diff characters by key."""
    results = []
    exp_map = {c["key"]: c for c in expected}
    act_map = {c["key"]: c for c in actual}
    all_keys = sorted(set(list(exp_map.keys()) + list(act_map.keys())))

    for key in all_keys:
        exp = exp_map.get(key)
        act = act_map.get(key)
        if exp is None:
            results.append((key, [("_status", "", "EXTRA in actual")]))
            continue
        if act is None:
            results.append((key, [("_status", "MISSING from actual", "")]))
            continue
        diffs = []
        for field in ["level", "constellation", "ascension", "element"]:
            ev = exp.get(field)
            av = act.get(field)
            if ev is not None and ev != av:
                diffs.append((field, str(ev), str(av)))
        # Compare talents
        exp_t = exp.get("talent", {})
        act_t = act.get("talent", {})
        for tf in ["auto", "skill", "burst"]:
            ev = exp_t.get(tf)
            av = act_t.get(tf)
            if ev is not None and ev != av:
                diffs.append((f"talent.{tf}", str(ev), str(av)))
        if diffs:
            results.append((key, diffs))
    return results


def generate_report(actual_path, expected_path, images_dir="debug_images", no_images=False):
    actual = load_json(actual_path)
    expected = load_json(expected_path)

    lines = []
    lines.append("# Scan Diff Report\n")
    lines.append(f"- **Actual**: `{actual_path}`")
    lines.append(f"- **Expected**: `{expected_path}`")
    if not no_images:
        lines.append(f"- **Images**: `{images_dir}/`")
    lines.append("")

    # Load index maps for correlating output positions with debug image folders
    art_index_map = None if no_images else load_index_map(images_dir, "artifacts")
    wpn_index_map = None if no_images else load_index_map(images_dir, "weapons")

    # === ARTIFACTS ===
    exp_artifacts = expected.get("artifacts") or []
    act_artifacts = actual.get("artifacts") or []
    if not exp_artifacts or not act_artifacts:
        exp_artifacts = []
        act_artifacts = []


    # --- Duplicate detection ---
    def artifact_fingerprint(a):
        """Fingerprint an artifact for duplicate detection.

        Uses identity fields: set, slot, rarity, level, mainStat, substats (ordered).
        Excludes location/lock/astralMark which can differ for the same artifact.
        """
        subs = tuple(
            (s["key"], s["value"]) for s in (a.get("substats") or [])
        )
        return (
            a.get("setKey", ""),
            a.get("slotKey", ""),
            a.get("rarity", 0),
            a.get("level", 0),
            a.get("mainStatKey", ""),
            subs,
        )

    def find_duplicates(artifacts, label):
        """Find duplicate artifacts, return list of (fingerprint, indices) with count > 1."""
        from collections import Counter
        fp_to_indices = defaultdict(list)
        for i, a in enumerate(artifacts):
            fp_to_indices[artifact_fingerprint(a)].append(i)
        return [(fp, indices) for fp, indices in fp_to_indices.items() if len(indices) > 1]

    exp_dupes = find_duplicates(exp_artifacts, "expected")
    act_dupes = find_duplicates(act_artifacts, "actual")

    if exp_dupes or act_dupes:
        lines.append("### Duplicate artifacts\n")
        for label, dupes, artifacts_list in [
            ("Expected", exp_dupes, exp_artifacts),
            ("Actual", act_dupes, act_artifacts),
        ]:
            if not dupes:
                continue
            total_duped = sum(len(indices) for _, indices in dupes)
            lines.append(f"**{label}**: {len(dupes)} duplicated artifacts ({total_duped} total instances)\n")
            for fp, indices in sorted(dupes, key=lambda x: -len(x[1]))[:20]:
                set_key, slot_key, rarity, level, main_stat, subs = fp
                subs_str = ", ".join(f"{k}={v}" for k, v in subs)
                locations = [artifacts_list[i].get("location", "") for i in indices]
                loc_str = ", ".join(f'"{l}"' if l else '""' for l in locations)
                lines.append(
                    f"- {len(indices)}× [{', '.join(f'{i:04d}' for i in indices)}] "
                    f"{set_key}/{slot_key} {rarity}★ lv{level} {main_stat} "
                    f"[{subs_str}] locations=[{loc_str}]"
                )
            if len(dupes) > 20:
                lines.append(f"- *... and {len(dupes) - 20} more*")
            lines.append("")

    artifact_diffs = diff_artifacts(exp_artifacts, act_artifacts)

    lines.append(f"## Artifacts ({len(act_artifacts)} scanned, {len(exp_artifacts)} expected, {len(artifact_diffs)} issues)\n")

    # Per-field summary — separate non-stat fields (top 10) from stat fields (top 3)
    a_field_counts = defaultdict(int)
    for _, _, _, diffs, *_ in artifact_diffs:
        for field, _, _ in diffs:
            if field != "_status":
                a_field_counts[field] += 1

    stat_fields = {f for f in a_field_counts
                   if ("substats" in f or "unactivated" in f) and not f.endswith(".count")}
    non_stat_fields = {f for f in a_field_counts if f not in stat_fields}

    if non_stat_fields:
        lines.append("### Non-stat field summary (top 10)\n")
        lines.append("| Field | Count |")
        lines.append("|-------|-------|")
        for field, count in sorted(
            [(f, a_field_counts[f]) for f in non_stat_fields], key=lambda x: -x[1]
        )[:10]:
            lines.append(f"| {field} | {count} |")
        lines.append("")

    if stat_fields:
        lines.append("### Stat field summary (top 3)\n")
        lines.append("| Field | Count |")
        lines.append("|-------|-------|")
        for field, count in sorted(
            [(f, a_field_counts[f]) for f in stat_fields], key=lambda x: -x[1]
        )[:3]:
            lines.append(f"| {field} | {count} |")
        remaining = len(stat_fields) - 3
        if remaining > 0:
            total_stat_errors = sum(a_field_counts[f] for f in stat_fields)
            top3_errors = sum(c for _, c in sorted(
                [(f, a_field_counts[f]) for f in stat_fields], key=lambda x: -x[1]
            )[:3])
            lines.append(f"| *... {remaining} more stat fields* | *{total_stat_errors - top3_errors} total* |")
        lines.append("")

    # Separate EXTRA/MISSING from real diffs, then categorize real diffs into tiers
    if artifact_diffs:
        non_stat_fields = {"setKey", "slotKey", "mainStatKey", "level", "rarity", "location", "lock",
                           "astralMark", "elixirCrafted"}
        cat_extra = []     # EXTRA in actual (scanned but not in GT)
        cat_missing = []   # MISSING from actual (in GT but not scanned)
        cat_nonstat = []   # has non-stat field diffs (incl. substats.count)
        cat_statkey = []   # has missing/extra substat keys but no non-stat
        cat_statval = []   # only substat value diffs

        for entry in sorted(artifact_diffs, key=lambda x: x[0]):
            idx, set_key, slot_key, diffs, exp_art, act_art = entry

            is_extra = any(f == "_status" and "EXTRA" in av for f, ev, av in diffs)
            is_missing = any(f == "_status" and "MISSING" in ev for f, ev, av in diffs)
            if is_extra:
                cat_extra.append(entry)
                continue
            if is_missing:
                cat_missing.append(entry)
                continue

            real_fields = [(f, ev, av) for f, ev, av in diffs if f != "_status"]

            has_non_stat = any(
                f in non_stat_fields or f.endswith(".count")
                for f, _, _ in real_fields
            )
            has_key_diff = any(
                f.endswith(".key") or ev == "missing" or av == "missing"
                for f, ev, av in real_fields
                if f not in non_stat_fields and not f.endswith(".count")
            )

            if has_non_stat:
                cat_nonstat.append(entry)
            elif has_key_diff:
                cat_statkey.append(entry)
            else:
                cat_statval.append(entry)

        def render_diff_entry(entry, lines_out):
            idx, set_key, slot_key, diffs, exp_art, act_art = entry
            is_missing = any(f == "_status" and "MISSING" in ev for f, ev, av in diffs)
            folder = None if (is_missing or no_images) else find_dump_folder(images_dir, "artifacts", idx, art_index_map)
            status = ""
            for field, ev, av in diffs:
                if field == "_status":
                    status = f" **{ev}{av}**"

            # Show elixirCrafted status (GT uses typo "elixerCrafted")
            gt_elixir = exp_art.get("elixirCrafted", exp_art.get("elixerCrafted", False))
            scan_elixir = act_art.get("elixirCrafted", act_art.get("elixerCrafted", False))
            elixir_tag = f" [elixir: gt={gt_elixir} scan={scan_elixir}]"

            diff_fields = [f for f, _, _ in diffs if f != "_status"]
            diff_summary = ", ".join(diff_fields[:5])
            if len(diff_fields) > 5:
                diff_summary += f" +{len(diff_fields)-5} more"

            lines_out.append(f"#### [{idx:04d}] {set_key} / {slot_key}{status}{elixir_tag} — {diff_summary}\n")

            # List diffs with per-field images inline
            for field, ev, av in diffs:
                if field == "_status":
                    continue
                lines_out.append(f"- **{field}**: expected=`{ev}` actual=`{av}`")
                if not no_images:
                    imgs = images_for_field(folder, field, "artifact", act_art=act_art)
                    for img in imgs:
                        lines_out.append(f"  - {img}")

            # Full screenshot in collapsible section
            if not no_images and folder:
                full_ref = image_ref(folder, "full.png")
                if full_ref:
                    lines_out.append(f"\n<details><summary>Full screenshot</summary>\n\n{full_ref}\n\n</details>")
            lines_out.append("")

        # Extra in actual (scanned but not in GT)
        if cat_extra:
            lines.append(f"### Extra in actual ({len(cat_extra)} items)\n")
            for entry in cat_extra:
                render_diff_entry(entry, lines)

        # Missing from actual (in GT but not scanned)
        if cat_missing:
            lines.append(f"### Missing from actual ({len(cat_missing)} items)\n")
            for entry in cat_missing:
                render_diff_entry(entry, lines)

        # Tier 1: Non-stat diffs (ALL)
        if cat_nonstat:
            lines.append(f"### Tier 1: Non-stat field diffs ({len(cat_nonstat)} items)\n")
            for entry in cat_nonstat:
                render_diff_entry(entry, lines)

        # Tier 2: Stat-key diffs (ALL, capped at 50)
        if cat_statkey:
            show = min(len(cat_statkey), 50)
            lines.append(f"### Tier 2: Stat-key diffs ({len(cat_statkey)} items, showing {show})\n")
            for i, entry in enumerate(cat_statkey):
                if i >= 50:
                    lines.append(f"\n*... and {len(cat_statkey) - 50} more*\n")
                    break
                render_diff_entry(entry, lines)

        # Tier 3: Stat-value only diffs (first 30)
        if cat_statval:
            show = min(len(cat_statval), 30)
            lines.append(f"### Tier 3: Stat-value only diffs ({len(cat_statval)} items, showing {show})\n")
            for i, entry in enumerate(cat_statval):
                if i >= 30:
                    lines.append(f"\n*... and {len(cat_statval) - 30} more*\n")
                    break
                render_diff_entry(entry, lines)

    # === WEAPONS ===
    exp_weapons = expected.get("weapons") or []
    act_weapons = actual.get("weapons") or []
    if exp_weapons and act_weapons:
        weapon_diffs = diff_weapons(exp_weapons, act_weapons)
        lines.append(f"## Weapons ({len(act_weapons)} scanned, {len(exp_weapons)} expected, {len(weapon_diffs)} issues)\n")

        if weapon_diffs:
            w_field_counts = defaultdict(int)
            for _, _, diffs in weapon_diffs:
                for field, _, _ in diffs:
                    if field != "_status":
                        w_field_counts[field] += 1
            if w_field_counts:
                lines.append("### Field summary\n")
                lines.append("| Field | Count |")
                lines.append("|-------|-------|")
                for field, count in sorted(w_field_counts.items(), key=lambda x: -x[1]):
                    lines.append(f"| {field} | {count} |")
                lines.append("")

            for idx, key, diffs in sorted(weapon_diffs, key=lambda x: x[0]):
                status = ""
                for field, ev, av in diffs:
                    if field == "_status":
                        status = f" **{ev}{av}**"
                diff_fields = [f for f, _, _ in diffs if f != "_status"]
                diff_summary = ", ".join(diff_fields[:5])
                if len(diff_fields) > 5:
                    diff_summary += f" +{len(diff_fields)-5} more"

                lines.append(f"#### [{idx:04d}] {key}{status} — {diff_summary}\n")

                folder = None if no_images else find_dump_folder(images_dir, "weapons", idx, wpn_index_map)
                for field, ev, av in diffs:
                    if field == "_status":
                        continue
                    lines.append(f"- **{field}**: expected=`{ev}` actual=`{av}`")
                    if not no_images:
                        imgs = images_for_field(folder, field, "weapon")
                        for img in imgs:
                            lines.append(f"  - {img}")

                if not no_images and folder:
                    full_ref = image_ref(folder, "full.png")
                    if full_ref:
                        lines.append(f"\n<details><summary>Full screenshot</summary>\n\n{full_ref}\n\n</details>")
                lines.append("")

    # === CHARACTERS ===
    exp_characters = expected.get("characters") or []
    act_characters = actual.get("characters") or []
    if exp_characters and act_characters:
        char_diffs = diff_characters(exp_characters, act_characters)
        lines.append(f"## Characters ({len(act_characters)} scanned, {len(exp_characters)} expected, {len(char_diffs)} issues)\n")

        if char_diffs:
            c_field_counts = defaultdict(int)
            for _, diffs in char_diffs:
                for field, _, _ in diffs:
                    if field != "_status":
                        c_field_counts[field] += 1
            if c_field_counts:
                lines.append("### Field summary\n")
                lines.append("| Field | Count |")
                lines.append("|-------|-------|")
                for field, count in sorted(c_field_counts.items(), key=lambda x: -x[1]):
                    lines.append(f"| {field} | {count} |")
                lines.append("")

            for key, diffs in char_diffs:
                status = ""
                for field, ev, av in diffs:
                    if field == "_status":
                        status = f" **{ev}{av}**"
                diff_fields = [f for f, _, _ in diffs if f != "_status"]
                diff_summary = ", ".join(diff_fields[:5])
                if len(diff_fields) > 5:
                    diff_summary += f" +{len(diff_fields)-5} more"

                lines.append(f"#### {key}{status} — {diff_summary}\n")

                for field, ev, av in diffs:
                    if field == "_status":
                        continue
                    lines.append(f"- **{field}**: expected=`{ev}` actual=`{av}`")
                lines.append("")

    return "\n".join(lines)


def shift_lock_astral(artifacts, offset=-1):
    """Shift lock/astralMark fields by `offset` positions.

    Each artifact[i] gets lock/astral from artifact[i+offset].
    Out-of-bounds indices keep their original values.
    """
    n = len(artifacts)
    orig_lock = [a.get("lock", False) for a in artifacts]
    orig_astral = [a.get("astralMark", False) for a in artifacts]
    for i in range(n):
        src = i - offset  # src index whose lock/astral we take
        if 0 <= src < n:
            artifacts[i]["lock"] = orig_lock[src]
            artifacts[i]["astralMark"] = orig_astral[src]
    return artifacts


def main():
    images_dir = "debug_images"
    no_images = False
    do_shift_lock = False

    args = sys.argv[1:]
    if "--no-images" in args:
        no_images = True
        args.remove("--no-images")
    if "--shift-lock" in args:
        do_shift_lock = True
        args.remove("--shift-lock")

    if len(args) >= 2:
        actual_path = args[0]
        expected_path = args[1]
        if len(args) >= 4 and args[2] == "--images":
            images_dir = args[3]
    else:
        actual_path = find_latest_export()
        if actual_path is None:
            print("No good_export_*.json found. Run the scan first.")
            sys.exit(1)
        expected_path = "genshin_export.json"

    print(f"Actual:   {actual_path}")
    print(f"Expected: {expected_path}")
    if no_images:
        print("Images:   disabled")
    else:
        print(f"Images:   {images_dir}/")
    if do_shift_lock:
        print("Lock shift: ON (each artifact gets previous artifact's lock/astral)")

    # Apply lock shift to actual before diffing
    if do_shift_lock:
        actual_data = load_json(actual_path)
        if "artifacts" in actual_data:
            actual_data["artifacts"] = shift_lock_astral(actual_data["artifacts"], offset=-1)
        # Write to temp file and use that
        import tempfile
        tmp = tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False, encoding="utf-8")
        json.dump(actual_data, tmp, ensure_ascii=False)
        tmp.close()
        actual_path = tmp.name

    report = generate_report(actual_path, expected_path, images_dir, no_images=no_images)

    output = "diff_report.md"
    with open(output, "w", encoding="utf-8") as f:
        f.write(report)

    print(f"Report written to {output}")


if __name__ == "__main__":
    main()
