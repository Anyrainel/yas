"""Analyze whether the next digit can be recovered from a rounded display value.

For percentage stats (displayed at 1dp like "5.3%"), can we determine the 2nd
decimal place (e.g., is it 5.25 or 5.31)?

For flat stats (displayed as integer like "63"), can we determine the 1st
decimal place (e.g., is it 62.50 or 63.12)?

Method: for each (rarity, stat), enumerate ALL possible exact sums across
all roll counts 1-6, round to display, and check whether multiple distinct
"next-digit" values share the same display value.

Usage: python tools/analyze_ambiguity.py <capture.json>
"""

import json
import math
import sys
from itertools import combinations_with_replacement
from collections import defaultdict

ROLLS = {
    5: {
        "hp":        [209.13, 239.00, 268.88, 298.75],
        "hp_":       [4.08,   4.66,   5.25,   5.83],
        "atk":       [13.62,  15.56,  17.51,  19.45],
        "atk_":      [4.08,   4.66,   5.25,   5.83],
        "def":       [16.20,  18.52,  20.83,  23.15],
        "def_":      [5.10,   5.83,   6.56,   7.29],
        "eleMas":    [16.32,  18.65,  20.98,  23.31],
        "enerRech_": [4.53,   5.18,   5.83,   6.48],
        "critRate_": [2.72,   3.11,   3.50,   3.89],
        "critDMG_":  [5.44,   6.22,   6.99,   7.77],
    },
    4: {
        "hp":        [167.30, 191.20, 215.10, 239.00],
        "hp_":       [3.26,   3.73,   4.20,   4.66],
        "atk":       [10.89,  12.45,  14.00,  15.56],
        "atk_":      [3.26,   3.73,   4.20,   4.66],
        "def":       [12.96,  14.82,  16.67,  18.52],
        "def_":      [4.08,   4.66,   5.25,   5.83],
        "eleMas":    [13.06,  14.92,  16.79,  18.65],
        "enerRech_": [3.63,   4.14,   4.66,   5.18],
        "critRate_": [2.18,   2.49,   2.80,   3.11],
        "critDMG_":  [4.35,   4.97,   5.60,   6.22],
    },
}

PCT_STATS = {"hp_", "atk_", "def_", "enerRech_", "critRate_", "critDMG_"}


def round_half_up_1dp(v):
    return math.floor(v * 10 + 0.5 + 1e-9) / 10

def round_half_up_int(v):
    return math.floor(v + 0.5 + 1e-9)

def round_half_up_2dp(v):
    return math.floor(v * 100 + 0.5 + 1e-9) / 100

def round_half_up_1dp_from_int(v):
    """Round integer to 1dp (i.e., round to nearest 0.1)."""
    return math.floor(v * 10 + 0.5 + 1e-9) / 10


def build_tables():
    """For each (rarity, stat), build display_value → set of exact sums (across all roll counts)."""
    # Also build display_value → set of "next digit" values
    tables = {}

    for rarity in [4, 5]:
        for stat, tiers in ROLLS[rarity].items():
            is_pct = stat in PCT_STATS

            # display_val → set of (exact_sum, n_rolls)
            mapping = defaultdict(set)

            for n_rolls in range(1, 7):
                for combo in combinations_with_replacement(range(4), n_rolls):
                    exact = sum(tiers[i] for i in combo)

                    if is_pct:
                        display = round_half_up_1dp(exact)
                        # "Next digit" = 2dp value
                        next_digit_val = round_half_up_2dp(exact)
                    else:
                        display = round_half_up_int(exact)
                        # "Next digit" = 1dp value
                        next_digit_val = round_half_up_1dp_from_int(exact)

                    mapping[display].add(next_digit_val)

            tables[(rarity, stat)] = {d: sorted(vs) for d, vs in mapping.items()}

    return tables


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <capture.json>")
        sys.exit(1)

    with open(sys.argv[1]) as f:
        cap_data = json.load(f)

    cap_arts = [a for a in cap_data.get("artifacts", [])
                if any(s.get("rolls") for s in a["substats"])]

    print(f"Capture artifacts with rolls: {len(cap_arts)}")

    tables = build_tables()

    # --- Theoretical analysis (all possible display values) ---
    print(f"\n{'='*90}")
    print(f"THEORETICAL: for every achievable display value, is the next digit unique?")
    print(f"{'='*90}")

    print(f"\n  {'Rarity':>6s}  {'Stat':>12s}  {'Display vals':>12s}  {'Unique':>7s}  {'Ambig':>7s}  {'%Unique':>8s}  {'MaxCands':>8s}")
    print(f"  {'-'*6}  {'-'*12}  {'-'*12}  {'-'*7}  {'-'*7}  {'-'*8}  {'-'*8}")

    for rarity in [5, 4]:
        for stat in sorted(ROLLS[rarity].keys()):
            tbl = tables[(rarity, stat)]
            n_display = len(tbl)
            n_unique = sum(1 for vs in tbl.values() if len(vs) == 1)
            n_ambig = n_display - n_unique
            max_cands = max(len(vs) for vs in tbl.values())
            pct = n_unique / n_display * 100 if n_display > 0 else 0
            print(f"  {rarity:>6d}  {stat:>12s}  {n_display:>12d}  {n_unique:>7d}  {n_ambig:>7d}  {pct:>7.1f}%  {max_cands:>8d}")

    # --- Show ambiguous display values ---
    print(f"\n{'='*90}")
    print(f"AMBIGUOUS DISPLAY VALUES (next digit has multiple candidates)")
    print(f"{'='*90}")

    for rarity in [5, 4]:
        for stat in sorted(ROLLS[rarity].keys()):
            is_pct = stat in PCT_STATS
            tbl = tables[(rarity, stat)]
            ambig = {d: vs for d, vs in tbl.items() if len(vs) > 1}
            if not ambig:
                continue
            print(f"\n  --- {rarity}★ {stat} ({len(ambig)} ambiguous out of {len(tbl)}) ---")
            for d in sorted(ambig.keys()):
                vs = ambig[d]
                if is_pct:
                    d_str = f"{d:.1f}%"
                    cands = ", ".join(f"{v:.2f}" for v in vs)
                else:
                    d_str = f"{d:.0f}"
                    cands = ", ".join(f"{v:.1f}" for v in vs)
                print(f"    {d_str:>10s} → [{cands}]")

    # --- Real data: how often is the next digit recoverable? ---
    print(f"\n{'='*90}")
    print(f"REAL DATA: how often can we recover the next digit?")
    print(f"{'='*90}")

    total = 0
    unique = 0
    ambiguous = 0
    not_found = 0
    deltas = []  # (max-min)/min * 100% for ambiguous cases
    stat_counts = defaultdict(lambda: {"total": 0, "unique": 0, "deltas": []})

    for art in cap_arts:
        rarity = art["rarity"]
        for sub in art["substats"]:
            key = sub["key"]
            rolls = sub.get("rolls", [])
            if not rolls:
                continue

            is_pct = key in PCT_STATS
            exact = sum(rolls)
            display = round_half_up_1dp(exact) if is_pct else round_half_up_int(exact)

            tbl = tables.get((rarity, key))
            if tbl is None:
                not_found += 1
                continue

            candidates = tbl.get(display)
            if candidates is None:
                not_found += 1
                continue

            total += 1
            sc = stat_counts[key]
            sc["total"] += 1

            if len(candidates) == 1:
                unique += 1
                sc["unique"] += 1
            else:
                ambiguous += 1
                lo, hi = candidates[0], candidates[-1]
                d = (hi - lo) / lo * 100 if lo > 0 else 0
                deltas.append(d)
                sc["deltas"].append(d)

    print(f"\n  Total substats:  {total}")
    print(f"  Next digit unique: {unique} ({unique/total*100:.1f}%)")
    print(f"  Ambiguous:         {ambiguous} ({ambiguous/total*100:.1f}%)")
    if not_found:
        print(f"  Not found:         {not_found}")

    if deltas:
        deltas.sort()
        print(f"\n  Confidence range for ambiguous cases (delta = (max-min)/min × 100%):")
        print(f"    Min:  {deltas[0]:.4f}%")
        print(f"    P25:  {deltas[len(deltas)//4]:.4f}%")
        print(f"    P50:  {deltas[len(deltas)//2]:.4f}%")
        print(f"    P75:  {deltas[3*len(deltas)//4]:.4f}%")
        print(f"    P95:  {deltas[int(len(deltas)*0.95)]:.4f}%")
        print(f"    Max:  {deltas[-1]:.4f}%")

    print(f"\n  {'Stat':>12s}  {'Total':>6s}  {'Unique':>7s}  {'Ambig':>7s}  {'%Unique':>8s}  {'MedΔ%':>8s}  {'MaxΔ%':>8s}")
    print(f"  {'-'*12}  {'-'*6}  {'-'*7}  {'-'*7}  {'-'*8}  {'-'*8}  {'-'*8}")
    for key in sorted(stat_counts.keys()):
        sc = stat_counts[key]
        t = sc["total"]
        u = sc["unique"]
        a = t - u
        ds = sorted(sc["deltas"])
        med = ds[len(ds)//2] if ds else 0
        mx = ds[-1] if ds else 0
        print(f"  {key:>12s}  {t:>6d}  {u:>7d}  {a:>7d}  {u/t*100:>7.1f}%  {med:>7.4f}%  {mx:>7.4f}%")


if __name__ == "__main__":
    main()
