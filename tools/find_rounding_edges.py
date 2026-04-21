"""Find substat roll combinations that land exactly on rounding boundaries.

Uses integer arithmetic (×100 "cents") — no float issues.
Only shows cases where banker's rounding and half-up rounding disagree.
Groups compositions that produce the same sum.

Usage: python tools/find_rounding_edges.py
"""

from itertools import combinations_with_replacement

# Tier values in display units: [0.7x, 0.8x, 0.9x, 1.0x]
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
TIER_NAMES = ["0.7x", "0.8x", "0.9x", "1.0x"]


def to_cents(v):
    return round(v * 100)


def paper_round_pct(cents):
    """Round cents to 1dp display. Returns (banker's, half-up) as strings."""
    # 1dp boundary: cents mod 10 == 5
    r = cents % 10
    base = cents - r
    if r < 5:
        val = base
    elif r > 5:
        val = base + 10
    else:
        # Midpoint — both methods differ here
        bankers = base if ((base // 10) % 2 == 0) else base + 10
        half_up = base + 10
        return f"{bankers / 100:.1f}%", f"{half_up / 100:.1f}%"
    s = f"{val / 100:.1f}%"
    return s, s  # agree


def paper_round_flat(cents):
    """Round cents to integer display. Returns (banker's, half-up) as strings."""
    r = cents % 100
    base = cents - r
    if r < 50:
        val = base
    elif r > 50:
        val = base + 100
    else:
        bankers = base if ((base // 100) % 2 == 0) else base + 100
        half_up = base + 100
        return str(bankers // 100), str(half_up // 100)
    s = str(val // 100)
    return s, s


def main():
    # Collect: (rarity, stat, sum_cents, bankers, half_up) → list of compositions
    from collections import defaultdict
    groups = defaultdict(list)

    for rarity in [5, 4]:
        for stat, tiers in ROLLS[rarity].items():
            is_pct = stat in PCT_STATS
            tiers_cents = [to_cents(t) for t in tiers]

            for n_rolls in range(1, 7):
                for combo in combinations_with_replacement(range(4), n_rolls):
                    cents = sum(tiers_cents[i] for i in combo)

                    if is_pct:
                        if cents % 10 != 5:
                            continue
                        bankers, half_up = paper_round_pct(cents)
                    else:
                        if cents % 100 != 50:
                            continue
                        bankers, half_up = paper_round_flat(cents)

                    if bankers == half_up:
                        continue  # not a disagreement

                    tier_counts = [0, 0, 0, 0]
                    for i in combo:
                        tier_counts[i] += 1
                    desc = " + ".join(
                        f"{tier_counts[i]}×{tiers[i]}({TIER_NAMES[i]})"
                        for i in range(4) if tier_counts[i] > 0
                    )

                    key = (rarity, stat, cents)
                    groups[key].append((n_rolls, desc))

    # Print grouped
    entries = sorted(groups.items(), key=lambda x: (x[0][0], x[0][1], x[0][2]))

    print(f"Rounding boundary cases where banker's ≠ half-up ({len(entries)} distinct values)\n")

    cur_header = None
    for (rarity, stat, cents), combos in entries:
        is_pct = stat in PCT_STATS
        if is_pct:
            bankers, half_up = paper_round_pct(cents)
            exact = f"{cents / 100:.2f}%"
        else:
            bankers, half_up = paper_round_flat(cents)
            exact = f"{cents / 100:.2f}"

        header = f"{rarity}★ {stat}"
        if header != cur_header:
            if cur_header is not None:
                print()
            print(f"--- {header} ---")
            cur_header = header

        print(f"  {exact}  banker's→{bankers}  half-up→{half_up}  ({len(combos)} compositions)")
        for n_rolls, desc in combos:
            print(f"    {n_rolls} rolls: {desc}")

    print(f"\nTotal: {sum(len(v) for v in groups.values())} compositions across {len(entries)} boundary values")


if __name__ == "__main__":
    main()
