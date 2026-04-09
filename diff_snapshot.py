"""Compare /artifacts snapshot against groundtruth export.

Matches items by identity (set, slot, rarity, level, main stat, substats,
elixirCrafted, unactivatedSubstats) with 0.1 value tolerance, then reports
field-level diffs on lock / astralMark / location / totalRolls / initialValue.
"""
import json, sys
from collections import Counter, defaultdict

SNAP_PATH = "artifacts_snapshot.json"
GT_PATH = r"F:\Codes\genshin\irminsul\genshin_export_2026-04-09_02-32.json"

def load():
    snap = json.load(open(SNAP_PATH))
    gt_full = json.load(open(GT_PATH))
    gt = gt_full["artifacts"]
    return snap, gt

def elixir(a):
    return a.get("elixirCrafted", a.get("elixerCrafted", False))

def round_val(v):
    # bucket values into 0.1-wide buckets; store bucket that value falls in.
    # we'll allow ±1 bucket during matching
    return round(v * 10)

def sub_sig(subs):
    # sorted (key, rounded_value_tenths) for order-independent signature
    return tuple(sorted((s["key"], round_val(s["value"])) for s in subs))

def sub_sig_variants(subs):
    # Each substat value can be off by up to 0.1 (±1 in tenths). Generate all
    # 3^n signature variants for substats with non-integer rounding.
    keys = [s["key"] for s in subs]
    vals = [round_val(s["value"]) for s in subs]
    variants = [[]]
    for v in vals:
        new = []
        for var in variants:
            for d in (-1, 0, 1):
                new.append(var + [v + d])
        variants = new
    return {tuple(sorted(zip(keys, vs))) for vs in variants}

def identity_key(a, vals_override=None):
    subs = a["substats"]
    unact = a.get("unactivatedSubstats", [])
    return (
        a["setKey"], a["slotKey"], a["rarity"], a["level"],
        a["mainStatKey"], elixir(a),
        sub_sig(subs) if vals_override is None else vals_override[0],
        sub_sig(unact) if vals_override is None else vals_override[1],
    )

def main():
    snap, gt = load()
    print(f"snapshot: {len(snap)} artifacts")
    print(f"groundtruth: {len(gt)} artifacts")
    print(f"snapshot lock dist:   {Counter(a['lock'] for a in snap)}")
    print(f"groundtruth lock dist:{Counter(a['lock'] for a in gt)}")
    print()

    # Group GT by a coarse key (everything except substat values) for fuzzy sub-matching.
    def coarse(a):
        return (a["setKey"], a["slotKey"], a["rarity"], a["level"],
                a["mainStatKey"], elixir(a),
                tuple(sorted(s["key"] for s in a["substats"])),
                tuple(sorted(s["key"] for s in a.get("unactivatedSubstats", []))))

    gt_by_coarse = defaultdict(list)
    for i, a in enumerate(gt):
        gt_by_coarse[coarse(a)].append(i)

    gt_used = [False] * len(gt)
    matched = []  # list of (snap_idx, gt_idx)
    unmatched_snap = []

    def subs_close(a, b):
        if len(a) != len(b): return False
        ka = sorted(a, key=lambda s: s["key"])
        kb = sorted(b, key=lambda s: s["key"])
        for sa, sb in zip(ka, kb):
            if sa["key"] != sb["key"]: return False
            if abs(sa["value"] - sb["value"]) > 0.1 + 1e-6: return False
        return True

    for si, sa in enumerate(snap):
        candidates = gt_by_coarse.get(coarse(sa), [])
        best = None
        for gi in candidates:
            if gt_used[gi]: continue
            ga = gt[gi]
            if subs_close(sa["substats"], ga["substats"]) and \
               subs_close(sa.get("unactivatedSubstats", []),
                          ga.get("unactivatedSubstats", [])):
                best = gi
                break
        if best is not None:
            gt_used[best] = True
            matched.append((si, best))
        else:
            unmatched_snap.append(si)

    unmatched_gt = [i for i, u in enumerate(gt_used) if not u]

    print(f"matched:       {len(matched)}")
    print(f"snap only:     {len(unmatched_snap)}")
    print(f"gt only:       {len(unmatched_gt)}")
    print()

    # Field-level diffs on matched pairs
    diffs = {
        "lock": [],
        "astralMark": [],
        "location": [],
        "totalRolls": [],
        "substat_value": [],   # within tolerance but different
    }
    for si, gi in matched:
        sa, ga = snap[si], gt[gi]
        if sa["lock"] != ga["lock"]:
            diffs["lock"].append((si, gi, sa["lock"], ga["lock"]))
        sa_am = sa.get("astralMark", False)
        ga_am = ga.get("astralMark", False)
        if sa_am != ga_am:
            diffs["astralMark"].append((si, gi, sa_am, ga_am))
        if sa.get("location", "") != ga.get("location", ""):
            diffs["location"].append((si, gi, sa.get("location",""), ga.get("location","")))
        sa_tr = sa.get("totalRolls")
        ga_tr = ga.get("totalRolls")
        if sa_tr != ga_tr:
            diffs["totalRolls"].append((si, gi, sa_tr, ga_tr))
        # substat value micro-diffs
        for x, y in zip(sorted(sa["substats"], key=lambda s: s["key"]),
                        sorted(ga["substats"], key=lambda s: s["key"])):
            if abs(x["value"] - y["value"]) > 1e-9:
                diffs["substat_value"].append((si, gi, x["key"], x["value"], y["value"]))
                break

    print("=== field-level diffs on matched pairs ===")
    for k, v in diffs.items():
        print(f"  {k}: {len(v)}")
    print()

    # Lock diff breakdown
    if diffs["lock"]:
        snap_false_gt_true = sum(1 for _,_,s,g in diffs["lock"] if not s and g)
        snap_true_gt_false = sum(1 for _,_,s,g in diffs["lock"] if s and not g)
        print(f"  lock breakdown: snap=F/gt=T: {snap_false_gt_true}  snap=T/gt=F: {snap_true_gt_false}")

    # astralMark breakdown
    if diffs["astralMark"]:
        sf_gt = sum(1 for _,_,s,g in diffs["astralMark"] if not s and g)
        st_gf = sum(1 for _,_,s,g in diffs["astralMark"] if s and not g)
        print(f"  astralMark breakdown: snap=F/gt=T: {sf_gt}  snap=T/gt=F: {st_gf}")
    print()

    # Cross-tab: lock diff vs astralMark
    if diffs["lock"]:
        # For each lock-diff, check if gt has astralMark
        snap_by_idx = {si: (si, gi) for si, gi in matched}
        lock_diff_sis = {si for si,_,_,_ in diffs["lock"]}
        gt_astral_in_lock_diff = 0
        for si,gi,sl,gl in diffs["lock"]:
            if gt[gi].get("astralMark", False):
                gt_astral_in_lock_diff += 1
        print(f"  of {len(diffs['lock'])} lock diffs, {gt_astral_in_lock_diff} have astralMark=true in GT")
    print()

    # Distribution of gt lock among unmatched_gt
    if unmatched_gt:
        c = Counter(gt[i]["lock"] for i in unmatched_gt)
        print(f"  unmatched GT lock dist: {c}")
    if unmatched_snap:
        c = Counter(snap[i]["lock"] for i in unmatched_snap)
        print(f"  unmatched snap lock dist: {c}")
    print()

    # Sample some lock diffs
    print("=== sample lock diffs (first 5) ===")
    for si, gi, sl, gl in diffs["lock"][:5]:
        sa = snap[si]
        ga = gt[gi]
        print(f"  snap[{si}] {sa['setKey']} {sa['slotKey']} +{sa['level']} {sa['mainStatKey']}")
        print(f"    snap lock={sl} astral={sa.get('astralMark')} loc={sa.get('location','')!r}")
        print(f"    gt   lock={gl} astral={ga.get('astralMark')} loc={ga.get('location','')!r}")

if __name__ == "__main__":
    main()
