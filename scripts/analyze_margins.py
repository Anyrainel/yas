"""
Analyze safety margins for color thresholds.

For each criterion (R>180, R-G>60, R-B>50, B>70), measure:
- Distribution of values for pixels that SHOULD match (padlock body)
- Distribution of values for pixels that SHOULD NOT match (card bg in same region)
- The gap between worst-case match and worst-case non-match
"""

import json
import os
import numpy as np
from PIL import Image
from multiprocessing import Pool, cpu_count
from functools import partial

BASE_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
GT_FILE = "F:/Codes/genshin/yas/scripts/grid_ground_truth.json"

SCALE = 2.0
GX = int(180.0 * SCALE)
GY = int(253.0 * SCALE)
OX = int(145.0 * SCALE)
OY = int(166.0 * SCALE)

def gc(r, c):
    return GX + c * OX, GY + r * OY


def analyze_image(scan_idx, page_items, gt_lock):
    """For each cell, collect pixel-level stats in the lock search region."""
    img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
    if not os.path.exists(img_path):
        return None

    page = scan_idx // 40
    items = page_items.get(page, [])
    if not items:
        return None

    img = np.array(Image.open(img_path))
    h, w = img.shape[:2]

    results = []

    for idx, row, col in items:
        if idx == scan_idx:
            continue

        cx, cy = gc(row, col)
        x1 = max(0, cx - 130)
        y1 = max(0, cy - 165)
        x2 = min(w, cx - 55)
        y2 = min(h, cy - 55)

        patch = img[y1:y2, x1:x2, :3]
        r = patch[:, :, 0].astype(np.int16)
        g = patch[:, :, 1].astype(np.int16)
        b = patch[:, :, 2].astype(np.int16)

        is_locked = gt_lock[idx]

        # Current pink criteria
        pink_mask = (r > 180) & ((r - g) > 60) & ((r - b) > 50) & (b > 70)
        non_pink_mask = ~pink_mask

        if is_locked:
            pink_pixels = np.sum(pink_mask)
            if pink_pixels > 0:
                # Stats of pixels that DO match (padlock body)
                pr = r[pink_mask]
                pg = g[pink_mask]
                pb = b[pink_mask]
                p_rg = pr - pg
                p_rb = pr - pb

                # Stats of pixels that DON'T match in the SAME region (card bg around lock)
                nr = r[non_pink_mask]
                ng = g[non_pink_mask]
                nb = b[non_pink_mask]

                results.append({
                    'idx': idx, 'locked': True, 'pink_count': int(pink_pixels),
                    # Pink pixel stats (what we're detecting)
                    'pink_R_min': int(pr.min()), 'pink_R_p5': int(np.percentile(pr, 5)),
                    'pink_G_max': int(pg.max()),
                    'pink_B_min': int(pb.min()), 'pink_B_p5': int(np.percentile(pb, 5)),
                    'pink_RG_min': int(p_rg.min()), 'pink_RG_p5': int(np.percentile(p_rg, 5)),
                    'pink_RB_min': int(p_rb.min()), 'pink_RB_p5': int(np.percentile(p_rb, 5)),
                    # Non-pink pixel stats in same region (what we must NOT detect)
                    'bg_R_max': int(nr.max()) if nr.size > 0 else 0,
                    'bg_B_max': int(nb.max()) if nb.size > 0 else 0,
                    'bg_RG_max': int((nr - ng).max()) if nr.size > 0 else 0,
                    'bg_RB_max': int((nr - nb).max()) if nr.size > 0 else 0,
                })
        else:
            # Unlocked: ALL pixels in region should be non-pink
            # Check how close any pixel comes to matching
            rg = r - g
            rb = r - b

            # For each criterion, find the closest pixel to the threshold
            results.append({
                'idx': idx, 'locked': False, 'pink_count': int(np.sum(pink_mask)),
                'closest_R': int(r.max()),
                'closest_B_above70': int(b[r > 150].max()) if np.any(r > 150) else 0,
                'closest_RG': int(rg.max()),
                'closest_RB': int(rb.max()),
                # How many pixels pass each individual criterion
                'pass_R180': int(np.sum(r > 180)),
                'pass_RG60': int(np.sum(rg > 60)),
                'pass_RB50': int(np.sum(rb > 50)),
                'pass_B70': int(np.sum(b > 70)),
                # How many pass 3 of 4 criteria
                'pass_3of4': int(np.sum(
                    ((r > 180).astype(int) + ((rg) > 60).astype(int) +
                     ((rb) > 50).astype(int) + (b > 70).astype(int)) >= 3
                )),
            })

    return results


def main():
    with open(GT_FILE) as f:
        gt_data = json.load(f)
    gt_items = gt_data['items']
    total_arts = len(gt_items)

    gt_lock = {g['idx']: g['lock'] for g in gt_items}

    page_items = {}
    for g in gt_items:
        i = g['idx']
        page = i // 40
        pos = i % 40
        row, col = pos // 8, pos % 8
        page_items.setdefault(page, []).append((i, row, col))

    print(f"Analyzing {total_arts} images...")

    worker = partial(analyze_image, page_items=page_items, gt_lock=gt_lock)

    all_locked = []
    all_unlocked = []

    with Pool(min(cpu_count(), 8)) as pool:
        for result_list in pool.imap_unordered(worker, range(total_arts), chunksize=20):
            if result_list is None:
                continue
            for r in result_list:
                if r['locked']:
                    all_locked.append(r)
                else:
                    all_unlocked.append(r)

    print(f"\nLocked samples: {len(all_locked):,}")
    print(f"Unlocked samples: {len(all_unlocked):,}")

    # === LOCKED items: how strong is the padlock signal? ===
    print(f"\n{'='*65}")
    print(f"  PADLOCK PIXEL VALUES (what we're detecting)")
    print(f"  These must stay ABOVE the thresholds")
    print(f"{'='*65}")

    pink_counts = [r['pink_count'] for r in all_locked]
    print(f"\n  Pink pixel count per cell:")
    print(f"    min={min(pink_counts)}  p1={int(np.percentile(pink_counts,1))}  "
          f"p5={int(np.percentile(pink_counts,5))}  median={int(np.median(pink_counts))}  "
          f"max={max(pink_counts)}")
    print(f"    Threshold: >=10.  Margin: {min(pink_counts) - 10} pixels above threshold")

    for field, threshold, direction in [
        ('pink_R_min', 180, 'above'),
        ('pink_RG_min', 60, 'above'),
        ('pink_RB_min', 50, 'above'),
        ('pink_B_min', 70, 'above'),
    ]:
        vals = [r[field] for r in all_locked if field in r]
        p5_field = field.replace('_min', '_p5')
        p5_vals = [r[p5_field] for r in all_locked if p5_field in r]
        margin = min(vals) - threshold
        print(f"\n  {field} (threshold: >{threshold}):")
        print(f"    absolute min={min(vals)}  p1={int(np.percentile(vals,1))}  "
              f"p5={int(np.percentile(vals,5))}  median={int(np.median(vals))}")
        if p5_vals:
            print(f"    per-cell p5:  min={min(p5_vals)}  median={int(np.median(p5_vals))}")
        print(f"    Margin: {margin} {'above' if margin > 0 else 'BELOW!'} threshold")

    # === UNLOCKED items: how far are they from triggering? ===
    print(f"\n{'='*65}")
    print(f"  UNLOCKED REGION VALUES (what we must NOT detect)")
    print(f"  These must stay BELOW the thresholds")
    print(f"{'='*65}")

    for field, desc in [
        ('closest_R', 'Max R in region (threshold: >180)'),
        ('closest_RG', 'Max R-G in region (threshold: >60)'),
        ('closest_RB', 'Max R-B in region (threshold: >50)'),
    ]:
        vals = [r[field] for r in all_unlocked if field in r]
        print(f"\n  {desc}:")
        print(f"    max={max(vals)}  p99={int(np.percentile(vals,99))}  "
              f"p95={int(np.percentile(vals,95))}  median={int(np.median(vals))}")

    # How many unlocked cells have pixels passing individual criteria?
    print(f"\n  Pixels passing individual criteria in unlocked cells:")
    for field in ['pass_R180', 'pass_RG60', 'pass_RB50', 'pass_B70', 'pass_3of4']:
        vals = [r[field] for r in all_unlocked if field in r]
        nonzero = sum(1 for v in vals if v > 0)
        print(f"    {field:12s}: {nonzero:,}/{len(vals):,} cells have >0 pixels  "
              f"(max={max(vals)}, p99={int(np.percentile(vals,99))})")

    fp_count = [r['pink_count'] for r in all_unlocked]
    nonzero_fp = sum(1 for v in fp_count if v > 0)
    print(f"\n  Pixels passing ALL 4 criteria (actual false pink):")
    print(f"    {nonzero_fp:,}/{len(all_unlocked):,} cells have >0  "
          f"max={max(fp_count)}  p99={int(np.percentile(fp_count,99))}")
    if nonzero_fp > 0:
        fp_nonzero_vals = [v for v in fp_count if v > 0]
        print(f"    Among those {nonzero_fp}: min={min(fp_nonzero_vals)} "
              f"max={max(fp_nonzero_vals)} mean={np.mean(fp_nonzero_vals):.1f}")

    # === THE KEY QUESTION: what if we relax thresholds? ===
    print(f"\n{'='*65}")
    print(f"  THRESHOLD RELAXATION ANALYSIS")
    print(f"  How much can we relax each criterion before errors appear?")
    print(f"{'='*65}")

    # Test progressively relaxed thresholds
    # For each relaxation, count how many unlocked cells would trigger
    for label, r_thr, rg_thr, rb_thr, b_thr in [
        ("Current",     180, 60, 50, 70),
        ("R>170",       170, 60, 50, 70),
        ("R>160",       160, 60, 50, 70),
        ("R-G>50",      180, 50, 50, 70),
        ("R-G>40",      180, 40, 50, 70),
        ("R-B>40",      180, 60, 40, 70),
        ("R-B>30",      180, 60, 30, 70),
        ("B>60",        180, 60, 50, 60),
        ("B>50",        180, 60, 50, 50),
        ("B>40",        180, 60, 50, 40),
        ("B>0 (no B)",  180, 60, 50, 0),
        ("All relaxed", 160, 40, 30, 50),
    ]:
        # For this we need to re-scan — but we can estimate from the per-criterion data
        # Actually let's just report how many unlocked cells have pass_3of4 > 0
        # This is approximate but informative
        pass

    # More precise: re-check against all images with relaxed thresholds
    # (fast because we only check unlocked items)
    print("\n  Testing relaxed thresholds against unlocked cells (count reaching >=10):")

    def test_relaxed(scan_idx):
        img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
        if not os.path.exists(img_path):
            return {}
        page = scan_idx // 40
        items = page_items.get(page, [])
        img = np.array(Image.open(img_path))
        h, w = img.shape[:2]
        results = {}
        for idx, row, col in items:
            if idx == scan_idx or gt_lock[idx]:
                continue
            cx, cy = gc(row, col)
            x1, y1 = max(0, cx-130), max(0, cy-165)
            x2, y2 = min(w, cx-55), min(h, cy-55)
            patch = img[y1:y2, x1:x2, :3]
            ri = patch[:,:,0].astype(np.int16)
            gi = patch[:,:,1].astype(np.int16)
            bi = patch[:,:,2].astype(np.int16)
            rg = ri - gi
            rb = ri - bi
            for label, rt, rgt, rbt, bt in [
                ("R>160,RG>40,RB>30,B>50", 160, 40, 30, 50),
                ("R>160,RG>50,RB>40,B>60", 160, 50, 40, 60),
                ("R>170,RG>50,RB>40,B>60", 170, 50, 40, 60),
                ("R>180,RG>60,RB>50,B>0",  180, 60, 50, 0),
                ("R>180,RG>50,RB>40,B>70", 180, 50, 40, 70),
                ("R>180,RG>60,RB>50,B>50", 180, 60, 50, 50),
                ("R>180,RG>60,RB>50,B>70 (current)", 180, 60, 50, 70),
            ]:
                mask = (ri > rt) & (rg > rgt) & (rb > rbt) & (bi > bt)
                cnt = int(np.sum(mask))
                if cnt >= 10:
                    results.setdefault(label, 0)
                    results[label] += 1
        return results

    totals = {}
    with Pool(min(cpu_count(), 8)) as pool:
        for res in pool.imap_unordered(test_relaxed, range(total_arts), chunksize=20):
            for k, v in res.items():
                totals[k] = totals.get(k, 0) + v

    total_unlocked_tests = len(all_unlocked)
    for label in [
        "R>160,RG>40,RB>30,B>50",
        "R>160,RG>50,RB>40,B>60",
        "R>170,RG>50,RB>40,B>60",
        "R>180,RG>50,RB>40,B>70",
        "R>180,RG>60,RB>50,B>50",
        "R>180,RG>60,RB>50,B>0",
        "R>180,RG>60,RB>50,B>70 (current)",
    ]:
        fp = totals.get(label, 0)
        print(f"    {label:42s}: {fp:5d} FP / {total_unlocked_tests:,} unlocked = "
              f"{fp/total_unlocked_tests*100:.4f}%")


if __name__ == "__main__":
    main()
