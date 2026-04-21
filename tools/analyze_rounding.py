"""Determine the game's rounding method from capture data with per-roll values.

Tests all 6 combinations: {f32, f64, integer} × {banker's, half-up}
against what the game actually shows (OCR scan value).

Usage: python tools/analyze_rounding.py <ocr_scan.json> <capture.json>
"""

import json
import math
import struct
import sys
from collections import defaultdict


PCT_STATS = {"hp_", "atk_", "def_", "enerRech_", "critRate_", "critDMG_"}


def to_cents(v):
    return round(v * 100)


def f32(x):
    return struct.unpack('f', struct.pack('f', x))[0]


def f32_sum(values):
    s = f32(0.0)
    for v in values:
        s = f32(s + f32(v))
    return s


# --- 6 rounding methods ---

def int_bankers_pct(cents):
    """Integer arithmetic, banker's rounding, percentage stat → 1dp display."""
    r = cents % 10
    base = cents - r
    if r < 5:
        return base / 100
    elif r > 5:
        return (base + 10) / 100
    else:
        return base / 100 if ((base // 10) % 2 == 0) else (base + 10) / 100

def int_half_up_pct(cents):
    r = cents % 10
    base = cents - r
    return (base + 10) / 100 if r >= 5 else base / 100

def int_bankers_flat(cents):
    r = cents % 100
    base = cents - r
    if r < 50:
        return base / 100
    elif r > 50:
        return (base + 100) / 100
    else:
        return base / 100 if ((base // 100) % 2 == 0) else (base + 100) / 100

def int_half_up_flat(cents):
    r = cents % 100
    base = cents - r
    return (base + 100) / 100 if r >= 50 else base / 100

def f64_bankers_pct(v):
    return round(v, 1)

def f64_half_up_pct(v):
    return math.floor(v * 10 + 0.5 + 1e-9) / 10

def f64_bankers_flat(v):
    return float(round(v))

def f64_half_up_flat(v):
    return math.floor(v + 0.5 + 1e-9)

def f32_bankers_pct(v):
    # Round f32 value with banker's
    return round(v, 1)

def f32_half_up_pct(v):
    return math.floor(v * 10 + 0.5 + 1e-9) / 10

def f32_bankers_flat(v):
    return float(round(v))

def f32_half_up_flat(v):
    return math.floor(v + 0.5 + 1e-9)


def match_artifacts(ocr_arts, cap_arts):
    def group_key(a):
        return (a["setKey"], a["slotKey"], a.get("rarity", 0),
                a.get("level", 0), a.get("mainStatKey", ""))

    ocr_groups = defaultdict(list)
    for a in ocr_arts:
        ocr_groups[group_key(a)].append(a)

    cap_groups = defaultdict(list)
    for a in cap_arts:
        cap_groups[group_key(a)].append(a)

    matched = []
    for key in cap_groups:
        caps = list(cap_groups[key])
        ocrs = list(ocr_groups.get(key, []))
        used_ocr = set()
        for ca in caps:
            best_idx, best_score = -1, 1e9
            for i, oa in enumerate(ocrs):
                if i in used_ocr:
                    continue
                csubs = {s["key"]: s["value"] for s in ca["substats"]}
                osubs = {s["key"]: s["value"] for s in oa["substats"]}
                if csubs.keys() != osubs.keys():
                    score = 10000
                else:
                    score = sum(abs(csubs[k] - osubs[k]) for k in csubs)
                if score < best_score:
                    best_score = score
                    best_idx = i
            if best_idx >= 0 and best_score < 100:
                matched.append((ca, ocrs[best_idx]))
                used_ocr.add(best_idx)

    return matched


METHOD_NAMES = [
    "int+banker",
    "int+half-up",
    "f64+banker",
    "f64+half-up",
    "f32+banker",
    "f32+half-up",
]


def compute_all_methods(rolls, is_pct):
    """Return dict of method_name → display_value for all 6 methods."""
    cents = sum(to_cents(r) for r in rolls)
    sum_f64 = sum(rolls)  # Python float = f64
    sum_f32 = f32_sum(rolls)

    if is_pct:
        return {
            "int+banker":  int_bankers_pct(cents),
            "int+half-up": int_half_up_pct(cents),
            "f64+banker":  f64_bankers_pct(sum_f64),
            "f64+half-up": f64_half_up_pct(sum_f64),
            "f32+banker":  f32_bankers_pct(sum_f32),
            "f32+half-up": f32_half_up_pct(sum_f32),
        }
    else:
        return {
            "int+banker":  int_bankers_flat(cents),
            "int+half-up": int_half_up_flat(cents),
            "f64+banker":  f64_bankers_flat(sum_f64),
            "f64+half-up": f64_half_up_flat(sum_f64),
            "f32+banker":  f32_bankers_flat(sum_f32),
            "f32+half-up": f32_half_up_flat(sum_f32),
        }


def main():
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} <ocr_scan.json> <capture.json>")
        sys.exit(1)

    with open(sys.argv[1]) as f:
        ocr_data = json.load(f)
    with open(sys.argv[2]) as f:
        cap_data = json.load(f)

    ocr_arts = ocr_data.get("artifacts", [])
    cap_arts = [a for a in cap_data.get("artifacts", [])
                if any(s.get("rolls") for s in a["substats"])]

    print(f"OCR artifacts: {len(ocr_arts)}")
    print(f"Capture artifacts with rolls: {len(cap_arts)}")

    matched = match_artifacts(ocr_arts, cap_arts)
    print(f"Matched pairs: {len(matched)}\n")

    # Per-method: count correct, wrong, and collect disagreement details
    method_correct = {m: 0 for m in METHOD_NAMES}
    method_wrong = {m: 0 for m in METHOD_NAMES}
    total_substats = 0
    # Cases where at least two methods disagree on output
    diagnostic_cases = []

    for cap_art, ocr_art in matched:
        ocr_subs = {s["key"]: s["value"] for s in ocr_art["substats"]}
        for sub in cap_art["substats"]:
            key = sub["key"]
            rolls = sub.get("rolls", [])
            if not rolls:
                continue
            ocr_val = ocr_subs.get(key)
            if ocr_val is None:
                continue

            total_substats += 1
            is_pct = key in PCT_STATS
            results = compute_all_methods(rolls, is_pct)

            tol = 0.05 if is_pct else 0.5
            values_set = set(round(v, 2) for v in results.values())

            for m in METHOD_NAMES:
                if abs(results[m] - ocr_val) < tol:
                    method_correct[m] += 1
                else:
                    method_wrong[m] += 1

            # Only interesting if methods disagree
            if len(values_set) > 1:
                diagnostic_cases.append({
                    "set": cap_art["setKey"],
                    "slot": cap_art["slotKey"],
                    "level": cap_art["level"],
                    "stat": key,
                    "rolls": rolls,
                    "ocr_val": ocr_val,
                    "results": results,
                })

    # --- Summary table ---
    print(f"Total substats: {total_substats}")
    print(f"Diagnostic cases (methods disagree): {len(diagnostic_cases)}\n")

    print("Method scores (higher = more matches with OCR):\n")
    print(f"  {'Method':<15s}  {'Correct':>7s}  {'Wrong':>7s}  {'Accuracy':>8s}")
    print(f"  {'-'*15}  {'-'*7}  {'-'*7}  {'-'*8}")
    for m in METHOD_NAMES:
        c = method_correct[m]
        w = method_wrong[m]
        pct = c / (c + w) * 100 if (c + w) > 0 else 0
        marker = " ← BEST" if w == 0 else ""
        print(f"  {m:<15s}  {c:>7d}  {w:>7d}  {pct:>7.3f}%{marker}")

    # --- Show cases that distinguish methods ---
    # Group diagnostic cases by which methods got it right vs wrong
    from collections import Counter
    pattern_counts = Counter()
    pattern_examples = {}

    for case in diagnostic_cases:
        ocr = case["ocr_val"]
        tol = 0.05 if case["stat"] in PCT_STATS else 0.5
        pattern = tuple(
            "✓" if abs(case["results"][m] - ocr) < tol else "✗"
            for m in METHOD_NAMES
        )
        pattern_counts[pattern] += 1
        if pattern not in pattern_examples:
            pattern_examples[pattern] = case

    if pattern_counts:
        print(f"\nDisagreement patterns ({len(pattern_counts)} distinct):\n")
        header = "  ".join(f"{m[:7]:>7s}" for m in METHOD_NAMES)
        print(f"  Count  {header}  Example")
        print(f"  -----  {'  '.join(['-'*7]*6)}  -------")

        for pattern, count in pattern_counts.most_common():
            cols = "  ".join(f"{p:>7s}" for p in pattern)
            ex = pattern_examples[pattern]
            rolls_str = "+".join(f"{r}" for r in ex["rolls"])
            print(f"  {count:>5d}  {cols}  {ex['stat']} {rolls_str}→OCR={ex['ocr_val']}")

    # --- Show individual cases where the best method(s) failed ---
    best_methods = [m for m in METHOD_NAMES if method_wrong[m] == min(method_wrong.values())]
    failures = [c for c in diagnostic_cases
                if any(abs(c["results"][m] - c["ocr_val"]) >= (0.05 if c["stat"] in PCT_STATS else 0.5)
                       for m in best_methods)]
    if failures:
        print(f"\nCases where even best method(s) fail ({len(failures)}):")
        for c in failures[:10]:
            is_pct = c["stat"] in PCT_STATS
            tol = 0.05 if is_pct else 0.5
            rolls_str = " + ".join(f"{r}" for r in c["rolls"])
            print(f"\n  {c['set']}/{c['slot']} lv{c['level']} — {c['stat']}")
            print(f"    rolls: {rolls_str}")
            print(f"    OCR: {c['ocr_val']}")
            for m in METHOD_NAMES:
                v = c["results"][m]
                ok = "✓" if abs(v - c["ocr_val"]) < tol else "✗"
                print(f"    {m:<15s} → {v}  {ok}")


if __name__ == "__main__":
    main()
