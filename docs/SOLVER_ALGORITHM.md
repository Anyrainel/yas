# Artifact Substat Roll Solver — Algorithm Reference

This document describes the constraint-solving algorithm used to validate and decompose artifact substat values into individual rolls. It is implementation-agnostic — you only need the per-roll tier values and the roll table.

## Background: Genshin Artifact Roll Mechanics

Each artifact substat is the **sum of 1–N individual rolls**, where each roll picks one of 4 tiers: `0.7×`, `0.8×`, `0.9×`, `1.0×` of the stat's max base value.

### Roll Tier Values

Each stat key has 4 possible single-roll values per rarity. For example, 5-star `critRate_`:

| Tier | Multiplier | Value |
|------|-----------|-------|
| 0    | 0.7×      | 2.72  |
| 1    | 0.8×      | 3.11  |
| 2    | 0.9×      | 3.50  |
| 3    | 1.0×      | 3.89  |

These are internal (exact) values. The game displays a **rounded** value:
- **Flat stats** (hp, atk, def, eleMas): rounded to integer (half-up)
- **Percent stats** (hp\_, atk\_, def\_, enerRech\_, critRate\_, critDMG\_): rounded to 1 decimal place (half-up)

The rounding happens on the **final sum**, not per roll.

### Init Count and Upgrades

- **5-star**: init = 3 or 4 substats. Max level = 20.
- **4-star**: init = 2 or 3 substats. Max level = 16.
- Every 4 levels, one upgrade occurs: either a new substat is added (if < 4) or an existing substat gets an extra roll.
- **Total rolls** = `init_count + floor(level / 4)`
- At max level (20 for 5★, 16 for 4★), all artifacts have exactly 4 visible substats.

### Unactivated Substats (待激活)

At level 0 with `init < max_init`, the artifact displays `init + 1` substats, where the last one is "inactive" (greyed out, with `(待激活)` appended). The inactive substat has a real roll value — it's the value that will become active on the first level-up. The solver treats it as a normal substat for validation purposes.

## Core Data Structure: The Roll Table

The roll table is a pre-computed lookup that maps `(stat_key, rarity, display_value) → roll_count_bitmask`.

Each entry is `(display_value × 10, bitmask)` where bit `(N-1)` is set if `N` rolls can produce that display value. The table is sorted by display value for binary search.

Example: `(78, 0b00000010)` means display value 7.8 is achievable with exactly 2 rolls.

The table is generated offline by exhaustively enumerating all possible sums of 1–8 rolls across all tier combinations, applying the game's exact float32 arithmetic and display rounding, and recording which roll counts can produce each unique display value.

**If you already have this table, the solver is straightforward. If not, you can generate it by:**
1. For each `N` from 1 to 8 (roll count):
2. Enumerate all combinations-with-replacement of N tiers from `{0.7×, 0.8×, 0.9×, 1.0×}`
3. Sum the tier values (using the stat's exact per-tier values)
4. Apply display rounding (integer for flat, 1dp for percent)
5. Record that this display value is achievable with N rolls

## The Solver Algorithm

### Input

```
rarity:    4 or 5
level:     int
substats:  [{ key: string, value: float, inactive: bool }]
```

### Step 1: Determine Search Space

```
max_level = 20 if rarity==5 else 16
level = clamp(level, 0, max_level)
upgrades = floor(level / 4)
```

Try init counts in preference order:
- **Level 0**: prefer higher init first (the line count IS the init count)
- **Level > 0**: prefer lower init first (init=3 is more common for 5★, better accuracy)

```
5-star: level==0 → try [4, 3], else → try [3, 4]
4-star: level==0 → try [3, 2], else → try [2, 3]
```

For each init count:

```
total_rolls = init_count + upgrades
adds = min(4 - init_count, upgrades)     — new substats added via level-ups
expected_substats = init_count + adds     — total visible substats
```

### Step 2: Handle Inactive Substats at Level 0

At level 0 with `init < max_init`, the artifact shows `init + 1` lines (the extra is inactive). Try two variants:
1. Select `expected + 1` substats with `total_rolls + 1` (counting inactive)
2. Select `expected` substats with `total_rolls` (ignoring inactive)

Prefer variant 1 (uses more data).

### Step 3: Validate Substats

For the given substats:

1. **Count check**: number of substats must equal expected count
2. **Uniqueness check**: no two substats can share the same key
3. **Roll table lookup**: for each substat, query the roll table with `(key, rarity, display_value)` to get valid roll counts. Filter to `[1, max_per_stat]` where `max_per_stat = total_rolls - (num_substats - 1)` (every other substat needs at least 1 roll).

If any substat has no valid roll counts from the table, this init count / variant is invalid — try the next.

### Step 4: Constraint Solving (Backtracking)

Given `N` substats, each with a list of valid roll counts, find an assignment where the sum equals `total_rolls`.

```
function find_assignment(valid_counts[][], total_rolls):
    assignment = [0] × N
    return backtrack(valid_counts, total_rolls, 0, assignment)

function backtrack(valid_counts, remaining, idx, assignment):
    if idx == N:
        return remaining == 0
    min_remaining_others = sum of min(valid_counts[i]) for i in [idx+1, N)
    for count in valid_counts[idx]:
        if count > remaining: skip
        if remaining - count < min_remaining_others: skip
        assignment[idx] = count
        if backtrack(valid_counts, remaining - count, idx+1, assignment):
            return true
    return false
```

The pruning (`min_remaining_others`) makes this fast — typical artifacts have 3-4 substats with 1-3 valid roll counts each, so the search space is tiny.

### Step 5: Reconstruct Precise Value

The solver now knows each substat's roll count. The goal is to recover the **exact pre-rounding value** — the internal sum the game computed before applying display rounding.

**The problem**: knowing the roll count `N` is not enough to recover the exact value, because multiple tier combinations can produce the same display value. For example, 2 rolls of `[0.9×, 0.9×]` and `[0.8×, 1.0×]` may round to the same display number.

**Enumerate and filter**: for each solved substat with roll count `N`:

1. Enumerate all combinations-with-replacement of `N` tiers from `{0.7×, 0.8×, 0.9×, 1.0×}`
2. For each combination, compute the internal sum using the **exact per-tier values** (the same float precision the game engine uses — ideally f32 to match the game's C# `float`)
3. Apply display rounding to the sum
4. Keep only combinations whose rounded sum matches the observed display value

If exactly **one unique internal sum** survives, that is the precise value. If multiple distinct sums round to the same display value, the precise value is **ambiguous** — you can report a range or the set of possible values, but cannot determine a single exact answer from the display value alone.

**Precision note**: the game engine uses C# `float` (IEEE 754 float32). If you want exact reproduction, perform the tier summation in f32. Using f64 throughout and rounding at the end is also acceptable — the two approaches agree in nearly all cases, but edge cases exist where f32 intermediate rounding produces a different display value than f64. If your use case tolerates ±0.1% error, f64 is fine.

**Example**: 5★ `critRate_` with display value `10.5` and roll count 3.
- Enumerate all 20 combinations of 3 tiers
- Compute each sum: e.g., `[0.8×, 0.9×, 1.0×]` → `3.11 + 3.50 + 3.89` = `10.50`
- Round to 1dp: `10.5` ✓
- If this is the only sum that rounds to `10.5`, the precise value is `10.50`

### Step 6: Compute Initial Value (Optional)

For each solved substat, determine the display-rounded value of its first roll:

- **1 roll**: `initial_value = display_value` (trivially the first roll's display value). Verify by checking which tier matches: `round(tier_val) == display_value`.
- **N rolls**: try each of the 4 tiers as the initial roll. For each, check if the remaining `N-1` rolls can produce a sum that, when added to the initial tier value, matches the display value after rounding. If exactly one tier works, report it. If ambiguous (multiple tiers work), report `None`.

The enumeration for N-1 remaining rolls uses combinations-with-replacement (monotonically non-decreasing tier indices to avoid counting permutations).

### Step 7: Determine Final Init Count

At level 0, the init count is adjusted based on inactive substats:

```
result_init = num_active_substats - count(inactive_substats)
```

At level > 0, use the init count from the search loop.

Final output per substat:

```
total_rolls    = result_init + upgrades
roll_count     = assigned rolls for this substat
precise_value  = exact internal sum (if unambiguous, else None/range)
initial_value  = display-rounded first roll tier (if uniquely determinable)
```

## Complete Pseudocode

```
function solve(rarity, level, substats):
    max_level = 20 if rarity==5 else 16
    max_init  = 4  if rarity==5 else 3

    level = clamp(level, 0, max_level)
    upgrades = level / 4

    init_order = [4,3] if level==0 else [3,4]  # (for 5-star)
    for init_count in init_order:
        total_rolls = init_count + upgrades
        expected = init_count + min(4 - init_count, upgrades)

        # At lv0 with room for inactive, try selecting one more substat
        if level==0 and init_count < max_init:
            variants = [(expected+1, total_rolls+1), (expected, total_rolls)]
        else:
            variants = [(expected, total_rolls)]

        for (exp, solve_total) in variants:
            if len(substats) != exp: continue
            if has_duplicate_keys(substats): continue

            max_per = solve_total - (len(substats) - 1)
            valid_counts = [roll_table_lookup(s.key, rarity, s.value, max_per) for s in substats]

            if any is empty: continue

            assignment = find_assignment(valid_counts, solve_total)
            if assignment found:
                # Reconstruct precise values, compute initial values, return
                for s, rolls in zip(substats, assignment):
                    s.precise_value = reconstruct_precise(s.key, rarity, s.value, rolls)
                    s.initial_value = compute_initial(s.key, rarity, s.value, rolls)
                return build_result(substats, assignment, level, init_count, upgrades)

    return None

function reconstruct_precise(key, rarity, display_value, roll_count):
    tiers = roll_tiers(key, rarity)  # [0.7×, 0.8×, 0.9×, 1.0×]
    is_pct = key.ends_with("_")
    matching_sums = set()

    for combo in combinations_with_replacement([0,1,2,3], roll_count):
        internal_sum = sum(tiers[t] for t in combo)   # use f32 for exact match
        if round_to_display(internal_sum, is_pct) == display_value:
            matching_sums.add(internal_sum)

    if len(matching_sums) == 1:
        return matching_sums.pop()    # unique precise value
    else:
        return matching_sums          # ambiguous — return all possibilities
```

## Key Properties

- **Sound**: the solver only accepts substat values that are mechanically possible
- **Complete**: tries all init count variants
- **Fast**: pruned backtracking over tiny search spaces (≤4 substats, ≤3 roll count options each)
- **Inactive-aware**: correctly models the extra visible substat at level 0
- **Precision recovery**: when the roll decomposition is unambiguous, recovers the exact pre-rounding internal value from the display-rounded input
