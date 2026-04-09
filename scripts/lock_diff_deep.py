"""Deep analysis: what characterizes false-unlocks in the snapshot?"""
import json
from collections import Counter, defaultdict

snap = json.load(open("artifacts_snapshot.json"))
gt = json.load(open(r"F:\Codes\genshin\irminsul\genshin_export_2026-04-09_02-32.json"))["artifacts"]

def elixir(a): return a.get("elixirCrafted", a.get("elixerCrafted", False))
def coarse(a):
    return (a["setKey"], a["slotKey"], a["rarity"], a["level"], a["mainStatKey"], elixir(a),
            tuple(sorted(s["key"] for s in a["substats"])),
            tuple(sorted(s["key"] for s in a.get("unactivatedSubstats", []))))

def subs_close(a, b):
    if len(a) != len(b): return False
    ka = sorted(a, key=lambda s: s["key"])
    kb = sorted(b, key=lambda s: s["key"])
    return all(sa["key"] == sb["key"] and abs(sa["value"] - sb["value"]) <= 0.11
               for sa, sb in zip(ka, kb))

gt_by_coarse = defaultdict(list)
for i, a in enumerate(gt): gt_by_coarse[coarse(a)].append(i)

gt_used = [False] * len(gt)
matched = []
for si, sa in enumerate(snap):
    cands = gt_by_coarse.get(coarse(sa), [])
    for gi in cands:
        if gt_used[gi]: continue
        if subs_close(sa["substats"], gt[gi]["substats"]) and \
           subs_close(sa.get("unactivatedSubstats", []), gt[gi].get("unactivatedSubstats", [])):
            gt_used[gi] = True
            matched.append((si, gi))
            break

# Classify lock diffs
false_unlocks = [(si,gi) for si,gi in matched if not snap[si]["lock"] and gt[gi]["lock"]]
false_locks   = [(si,gi) for si,gi in matched if snap[si]["lock"] and not gt[gi]["lock"]]
agree         = [(si,gi) for si,gi in matched if snap[si]["lock"] == gt[gi]["lock"]]

print(f"agree: {len(agree)}  false_unlocks: {len(false_unlocks)}  false_locks: {len(false_locks)}")
print()

# Equipped distribution
def is_equipped(a): return bool(a.get("location",""))
print("=== false-unlocks: equipped distribution (from GT) ===")
print(f"  equipped: {sum(1 for _,gi in false_unlocks if is_equipped(gt[gi]))}")
print(f"  not equipped: {sum(1 for _,gi in false_unlocks if not is_equipped(gt[gi]))}")
print()

print("=== agree (snap=gt=true): equipped distribution ===")
agree_locked = [(si,gi) for si,gi in agree if snap[si]["lock"]]
print(f"  total agree-locked: {len(agree_locked)}")
print(f"  equipped: {sum(1 for _,gi in agree_locked if is_equipped(gt[gi]))}")
print()

# Level distribution of false unlocks vs agree
print("=== level distribution of false-unlocks ===")
c = Counter(snap[si]["level"] for si,_ in false_unlocks)
for lvl in sorted(c): print(f"  +{lvl}: {c[lvl]}")
print()
print("=== level distribution of agree-locked ===")
c = Counter(snap[si]["level"] for si,_ in agree_locked)
for lvl in sorted(c): print(f"  +{lvl}: {c[lvl]}")
print()

# Rarity
print("=== rarity distribution of false-unlocks ===")
c = Counter(snap[si]["rarity"] for si,_ in false_unlocks)
print(" ", dict(c))
print("=== rarity of agree-locked ===")
c = Counter(snap[si]["rarity"] for si,_ in agree_locked)
print(" ", dict(c))
print()

# Intra-page position (cell 0..39) for false-unlocks
print("=== false-unlocks by intra-page cell position ===")
by_cell = Counter(si % 40 for si,_ in false_unlocks)
total_by_cell = Counter(si % 40 for si in range(len(snap)))
for c in range(40):
    row, col = c // 8, c % 8
    rate = by_cell[c] / max(total_by_cell[c], 1)
    print(f"  cell {c:2} (r{row} c{col}): {by_cell[c]:3}/{total_by_cell[c]:3} = {rate*100:5.1f}% false-unlock")
print()

# Detect astralMark correlation
gt_astral_in_fu = sum(1 for _,gi in false_unlocks if gt[gi].get("astralMark", False))
print(f"false-unlocks with GT astralMark=true: {gt_astral_in_fu}")
print()

# Look at whether astralMark was detected in snap when false-unlock
snap_astral_in_fu = sum(1 for si,_ in false_unlocks if snap[si].get("astralMark", False))
print(f"false-unlocks with SNAP astralMark=true: {snap_astral_in_fu}")
