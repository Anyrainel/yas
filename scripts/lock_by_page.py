"""Check if false-unlock rate is uniform across pages or concentrated on some."""
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
    ka = sorted(a, key=lambda s: s["key"]); kb = sorted(b, key=lambda s: s["key"])
    return all(sa["key"] == sb["key"] and abs(sa["value"] - sb["value"]) <= 0.11 for sa, sb in zip(ka, kb))

gt_by_coarse = defaultdict(list)
for i, a in enumerate(gt): gt_by_coarse[coarse(a)].append(i)
gt_used = [False] * len(gt)
gt_for = {}  # si -> gi
for si, sa in enumerate(snap):
    for gi in gt_by_coarse.get(coarse(sa), []):
        if gt_used[gi]: continue
        if subs_close(sa["substats"], gt[gi]["substats"]) and \
           subs_close(sa.get("unactivatedSubstats", []), gt[gi].get("unactivatedSubstats", [])):
            gt_used[gi] = True
            gt_for[si] = gi
            break

# Per-page accuracy (page = si // 40)
from collections import defaultdict
page_stats = defaultdict(lambda: {"total":0, "gt_lock":0, "snap_lock_correct":0, "snap_lock_wrong":0})
for si in range(len(snap)):
    if si not in gt_for: continue
    p = si // 40
    ps = page_stats[p]
    ps["total"] += 1
    ga = gt[gt_for[si]]
    if ga["lock"]:
        ps["gt_lock"] += 1
        if snap[si]["lock"]: ps["snap_lock_correct"] += 1
        else: ps["snap_lock_wrong"] += 1

print("page  total  gt_lock  correct  wrong   acc%")
for p in sorted(page_stats):
    ps = page_stats[p]
    acc = ps["snap_lock_correct"] / max(ps["gt_lock"], 1) * 100
    print(f"  {p:3}  {ps['total']:5}  {ps['gt_lock']:7}  {ps['snap_lock_correct']:7}  {ps['snap_lock_wrong']:5}  {acc:5.1f}")
