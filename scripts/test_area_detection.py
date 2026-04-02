"""
Test area-based icon detection using precise grid-relative coordinates.

Instead of searching large windows for specific pixel colors, this approach
crops a small region at the known icon center and classifies it by mean color.

Tests multiple crop sizes (6x6, 10x10, 16x16, 20x20) and compares accuracy
against ground truth.
"""

import json
import os
import sys
import time
import numpy as np
from PIL import Image
from multiprocessing import Pool
from collections import defaultdict

BASE_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
GT_FILE = "F:/Codes/genshin/yas/scripts/grid_ground_truth.json"

# Precise grid parameters at 4K (from grid_fit_clean.json)
GX_LOCK = 262.345   # first cell lock centroid X
GY_LOCK = 386.812   # first cell lock centroid Y
OX = 292.765         # cell spacing X
OY = 350.391         # cell spacing Y
COLS = 8
ROWS = 5
ITEMS_PER_PAGE = 40

# Pink pixel thresholds for scroll calibration
PINK_R_MIN = 180
PINK_RG_DIFF = 60
PINK_RB_DIFF = 50
PINK_B_MIN = 70
PINK_MIN_COUNT = 10  # minimum pink pixels to consider as lock icon

# Search window for scroll calibration (±40px from expected lock position)
CALIB_WINDOW = 40


def lock_pos(row, col):
    """Expected lock centroid at 4K, before scroll calibration."""
    return GX_LOCK + col * OX, GY_LOCK + row * OY


def calibrate_scroll(img_arr, page_items, gt_lock):
    """Find per-image Y scroll offset by finding pink centroids for locked items
    and computing median residual from expected positions."""
    h, w = img_arr.shape[:2]
    residuals = []

    for idx, row, col in page_items:
        if not gt_lock.get(idx, False):
            continue
        ex, ey = lock_pos(row, col)

        # Search window around expected lock position
        x1 = max(0, int(ex - CALIB_WINDOW))
        y1 = max(0, int(ey - CALIB_WINDOW))
        x2 = min(w, int(ex + CALIB_WINDOW))
        y2 = min(h, int(ey + CALIB_WINDOW))

        patch = img_arr[y1:y2, x1:x2, :3]
        r = patch[:, :, 0].astype(np.int16)
        g = patch[:, :, 1].astype(np.int16)
        b = patch[:, :, 2].astype(np.int16)
        mask = (r > PINK_R_MIN) & ((r - g) > PINK_RG_DIFF) & ((r - b) > PINK_RB_DIFF) & (b > PINK_B_MIN)
        count = np.sum(mask)
        if count < PINK_MIN_COUNT:
            continue
        ys, xs = np.where(mask)
        cy = float(np.mean(ys)) + y1
        cx = float(np.mean(xs)) + x1
        residuals.append(cy - ey)

    if len(residuals) >= 3:
        return float(np.median(residuals))
    return 0.0


def crop_mean_rgb(img_arr, cx, cy, size):
    """Crop a size x size region centered at (cx, cy), return mean RGB."""
    h, w = img_arr.shape[:2]
    half = size // 2
    x1 = max(0, int(round(cx)) - half)
    y1 = max(0, int(round(cy)) - half)
    x2 = min(w, x1 + size)
    y2 = min(h, y1 + size)
    if x2 <= x1 or y2 <= y1:
        return (0.0, 0.0, 0.0)
    patch = img_arr[y1:y2, x1:x2, :3].astype(np.float64)
    return tuple(patch.mean(axis=(0, 1)))


def classify_lock(mean_rgb):
    """Classify whether the crop at lock position indicates a lock icon.

    Lock body center colors (from analysis):
      - 5★ locked: R≈250, G≈136, B≈115
      - 4★ locked: R≈250, G≈136, B≈116
      - Empty 5★ (gold bg): R≈152, G≈100, B≈43
      - Empty 4★ (purple bg): R≈102, G≈91, B≈141
      - Dark square bg: brightness < 90

    Lock icon is distinctly pink: high R (>180), R-G > 50.
    """
    r, g, b = mean_rgb
    # Lock icon is pink/salmon: high R, moderate G and B, R dominates
    if r > 180 and (r - g) > 50:
        return True
    return False


def process_image(args):
    """Process a single full.png image. Returns per-crop-size results."""
    scan_idx, page_items_list, gt_lock_dict, crop_sizes = args

    img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
    if not os.path.exists(img_path):
        return None

    img_arr = np.array(Image.open(img_path))

    # Calibrate scroll for this image
    scroll_dy = calibrate_scroll(img_arr, page_items_list, gt_lock_dict)

    results = {}
    for sz in crop_sizes:
        results[sz] = {
            'tp': 0, 'fp': 0, 'fn': 0, 'tn': 0,
            'errors': [],  # (idx, pred, true, mean_rgb)
        }

    for idx, row, col in page_items_list:
        if idx == scan_idx:
            continue  # skip selected item (has highlight)

        true_lock = gt_lock_dict.get(idx)
        if true_lock is None:
            continue

        ex, ey = lock_pos(row, col)
        cy = ey + scroll_dy

        for sz in crop_sizes:
            mean_rgb = crop_mean_rgb(img_arr, ex, cy, sz)
            pred_lock = classify_lock(mean_rgb)

            r = results[sz]
            if true_lock:
                if pred_lock:
                    r['tp'] += 1
                else:
                    r['fn'] += 1
                    r['errors'].append((idx, False, True, mean_rgb))
            else:
                if pred_lock:
                    r['fp'] += 1
                    r['errors'].append((idx, True, False, mean_rgb))
                else:
                    r['tn'] += 1

    return results


def main():
    t0 = time.time()

    # Load ground truth
    with open(GT_FILE) as f:
        gt_data = json.load(f)
    gt_items = gt_data['items']
    gt_lock = {g['idx']: g['lock'] for g in gt_items}
    total_arts = len(gt_items)
    print(f"Ground truth: {total_arts} items, {sum(1 for g in gt_items if g['lock'])} locked")

    # Build page -> items mapping
    page_items = defaultdict(list)
    for g in gt_items:
        i = g['idx']
        page = i // ITEMS_PER_PAGE
        pos = i % ITEMS_PER_PAGE
        row, col = pos // COLS, pos % COLS
        page_items[page].append((i, row, col))

    crop_sizes = [6, 8, 10, 16, 20]

    # Build work items: one per scan_idx
    work = []
    for scan_idx in range(total_arts):
        page = scan_idx // ITEMS_PER_PAGE
        items = page_items.get(page, [])
        if items:
            work.append((scan_idx, items, gt_lock, crop_sizes))

    print(f"Processing {len(work)} images with 8 workers...")

    # Aggregate results per crop size
    agg = {sz: {'tp': 0, 'fp': 0, 'fn': 0, 'tn': 0, 'errors': []} for sz in crop_sizes}

    with Pool(8) as pool:
        for i, res in enumerate(pool.imap_unordered(process_image, work, chunksize=20)):
            if res is None:
                continue
            for sz in crop_sizes:
                for k in ('tp', 'fp', 'fn', 'tn'):
                    agg[sz][k] += res[sz][k]
                agg[sz]['errors'].extend(res[sz]['errors'])
            if (i + 1) % 500 == 0:
                elapsed = time.time() - t0
                print(f"  {i+1}/{len(work)} images done ({elapsed:.1f}s)")

    elapsed = time.time() - t0
    print(f"\nCompleted in {elapsed:.1f}s\n")

    # Report results
    print("=" * 80)
    print("LOCK DETECTION RESULTS — Area-based mean-color approach")
    print("=" * 80)

    for sz in crop_sizes:
        r = agg[sz]
        tp, fp, fn, tn = r['tp'], r['fp'], r['fn'], r['tn']
        total = tp + fp + fn + tn
        acc = (tp + tn) / total * 100 if total else 0
        prec = tp / (tp + fp) * 100 if (tp + fp) else 0
        rec = tp / (tp + fn) * 100 if (tp + fn) else 0
        f1 = 2 * prec * rec / (prec + rec) if (prec + rec) else 0

        print(f"\n--- Crop size: {sz}x{sz} ---")
        print(f"  Total samples: {total}")
        print(f"  TP={tp:,}  FP={fp:,}  FN={fn:,}  TN={tn:,}")
        print(f"  Accuracy:  {acc:.4f}%")
        print(f"  Precision: {prec:.4f}%")
        print(f"  Recall:    {rec:.4f}%")
        print(f"  F1:        {f1:.4f}%")

    # Error analysis for best crop size (pick by F1)
    best_sz = max(crop_sizes, key=lambda sz: (
        2 * agg[sz]['tp'] / (2 * agg[sz]['tp'] + agg[sz]['fp'] + agg[sz]['fn'])
        if (2 * agg[sz]['tp'] + agg[sz]['fp'] + agg[sz]['fn']) > 0 else 0
    ))

    print(f"\n{'=' * 80}")
    print(f"ERROR ANALYSIS — Best crop size: {best_sz}x{best_sz}")
    print(f"{'=' * 80}")

    errors = agg[best_sz]['errors']
    # Count errors per item idx
    err_by_idx = defaultdict(list)
    for idx, pred, true, rgb in errors:
        err_by_idx[idx].append((pred, true, rgb))

    fp_items = defaultdict(list)
    fn_items = defaultdict(list)
    for idx, errs in err_by_idx.items():
        for pred, true, rgb in errs:
            if pred and not true:
                fp_items[idx].append(rgb)
            elif not pred and true:
                fn_items[idx].append(rgb)

    print(f"\nFalse Positives: {len(fp_items)} unique items ({sum(len(v) for v in fp_items.values())} total occurrences)")
    if fp_items:
        print("  Sample FP items (idx: mean_rgb across occurrences):")
        for idx in sorted(fp_items.keys())[:15]:
            rgbs = fp_items[idx]
            avg_r = np.mean([r for r, g, b in rgbs])
            avg_g = np.mean([g for r, g, b in rgbs])
            avg_b = np.mean([b for r, g, b in rgbs])
            print(f"    idx={idx}: avg RGB=({avg_r:.1f}, {avg_g:.1f}, {avg_b:.1f}), count={len(rgbs)}")

    print(f"\nFalse Negatives: {len(fn_items)} unique items ({sum(len(v) for v in fn_items.values())} total occurrences)")
    if fn_items:
        print("  Sample FN items (idx: mean_rgb across occurrences):")
        for idx in sorted(fn_items.keys())[:15]:
            rgbs = fn_items[idx]
            avg_r = np.mean([r for r, g, b in rgbs])
            avg_g = np.mean([g for r, g, b in rgbs])
            avg_b = np.mean([b for r, g, b in rgbs])
            print(f"    idx={idx}: avg RGB=({avg_r:.1f}, {avg_g:.1f}, {avg_b:.1f}), count={len(rgbs)}")

    # Margin analysis: how close are correct classifications to the threshold?
    print(f"\n{'=' * 80}")
    print(f"MARGIN ANALYSIS — Crop size: {best_sz}x{best_sz}")
    print(f"{'=' * 80}")

    # Re-run on a sample to collect all RGB values for margin analysis
    # We'll collect from the error list + do a quick pass for stats
    # Actually, let's just add a pass that collects mean_rgb for all items
    # But that would be slow. Instead, let's sample some images.
    print("  (Collecting RGB distributions from first 100 images...)")
    locked_rgbs = []
    unlocked_rgbs = []
    sample_count = 0
    for scan_idx in range(min(200, total_arts)):
        img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
        if not os.path.exists(img_path):
            continue
        page = scan_idx // ITEMS_PER_PAGE
        items = page_items.get(page, [])
        img_arr = np.array(Image.open(img_path))
        scroll_dy = calibrate_scroll(img_arr, items, gt_lock)

        for idx, row, col in items:
            if idx == scan_idx:
                continue
            ex, ey = lock_pos(row, col)
            cy = ey + scroll_dy
            mean_rgb = crop_mean_rgb(img_arr, ex, cy, best_sz)
            if gt_lock.get(idx, False):
                locked_rgbs.append(mean_rgb)
            else:
                unlocked_rgbs.append(mean_rgb)
            sample_count += 1

    if locked_rgbs:
        lr = np.array(locked_rgbs)
        print(f"\n  Locked samples: {len(lr)}")
        print(f"    R: min={lr[:,0].min():.1f} mean={lr[:,0].mean():.1f} max={lr[:,0].max():.1f} std={lr[:,0].std():.1f}")
        print(f"    G: min={lr[:,1].min():.1f} mean={lr[:,1].mean():.1f} max={lr[:,1].max():.1f} std={lr[:,1].std():.1f}")
        print(f"    B: min={lr[:,2].min():.1f} mean={lr[:,2].mean():.1f} max={lr[:,2].max():.1f} std={lr[:,2].std():.1f}")
        rg_diff = lr[:,0] - lr[:,1]
        print(f"    R-G: min={rg_diff.min():.1f} mean={rg_diff.mean():.1f} max={rg_diff.max():.1f}")
        brightness = lr.mean(axis=1)
        print(f"    Brightness: min={brightness.min():.1f} mean={brightness.mean():.1f} max={brightness.max():.1f}")

    if unlocked_rgbs:
        ur = np.array(unlocked_rgbs)
        print(f"\n  Unlocked samples: {len(ur)}")
        print(f"    R: min={ur[:,0].min():.1f} mean={ur[:,0].mean():.1f} max={ur[:,0].max():.1f} std={ur[:,0].std():.1f}")
        print(f"    G: min={ur[:,1].min():.1f} mean={ur[:,1].mean():.1f} max={ur[:,1].max():.1f} std={ur[:,1].std():.1f}")
        print(f"    B: min={ur[:,2].min():.1f} mean={ur[:,2].mean():.1f} max={ur[:,2].max():.1f} std={ur[:,2].std():.1f}")
        rg_diff = ur[:,0] - ur[:,1]
        print(f"    R-G: min={rg_diff.min():.1f} mean={rg_diff.mean():.1f} max={rg_diff.max():.1f}")
        brightness = ur.mean(axis=1)
        print(f"    Brightness: min={brightness.min():.1f} mean={brightness.mean():.1f} max={brightness.max():.1f}")

    if locked_rgbs and unlocked_rgbs:
        # Find the closest locked/unlocked pair by R-G margin
        locked_rg_min = (np.array(locked_rgbs)[:,0] - np.array(locked_rgbs)[:,1]).min()
        unlocked_rg_max = (np.array(unlocked_rgbs)[:,0] - np.array(unlocked_rgbs)[:,1]).max()
        print(f"\n  Decision margin (R-G diff):")
        print(f"    Min locked R-G:   {locked_rg_min:.1f}")
        print(f"    Max unlocked R-G: {unlocked_rg_max:.1f}")
        print(f"    Gap:              {locked_rg_min - unlocked_rg_max:.1f}")

    # Also check what 6x6 sees
    print(f"\n  --- 6x6 analysis (why it fails) ---")
    locked_6 = []
    unlocked_6 = []
    for scan_idx in range(min(50, total_arts)):
        img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
        if not os.path.exists(img_path):
            continue
        page = scan_idx // ITEMS_PER_PAGE
        items = page_items.get(page, [])
        img_arr = np.array(Image.open(img_path))
        scroll_dy = calibrate_scroll(img_arr, items, gt_lock)
        for idx, row, col in items:
            if idx == scan_idx:
                continue
            ex, ey = lock_pos(row, col)
            cy = ey + scroll_dy
            mean_rgb = crop_mean_rgb(img_arr, ex, cy, 6)
            if gt_lock.get(idx, False):
                locked_6.append(mean_rgb)
            else:
                unlocked_6.append(mean_rgb)
    if locked_6:
        lr6 = np.array(locked_6)
        print(f"  Locked 6x6: R mean={lr6[:,0].mean():.1f}, R-G mean={(lr6[:,0]-lr6[:,1]).mean():.1f}, R-G min={(lr6[:,0]-lr6[:,1]).min():.1f}")
        # How many would pass threshold?
        passes = np.sum((lr6[:,0] > 180) & ((lr6[:,0] - lr6[:,1]) > 50))
        print(f"  Locked passing threshold: {passes}/{len(lr6)} ({passes/len(lr6)*100:.1f}%)")
    if unlocked_6:
        ur6 = np.array(unlocked_6)
        print(f"  Unlocked 6x6: R mean={ur6[:,0].mean():.1f}, R-G mean={(ur6[:,0]-ur6[:,1]).mean():.1f}, R-G max={(ur6[:,0]-ur6[:,1]).max():.1f}")

    # Compare against pink-pixel-counting baseline
    print(f"\n{'=' * 80}")
    print("COMPARISON SUMMARY")
    print(f"{'=' * 80}")
    print(f"  Pink-pixel-counting baseline (from ground truth build):")
    print(f"    Search window: 75x55px at 1080p (150x110 at 4K)")
    print(f"    Threshold: >= 10 pink pixels")
    print(f"    (Ground truth was built using majority vote of this method)")
    print(f"")
    print(f"  Area-based mean-color (this test):")
    for sz in crop_sizes:
        r = agg[sz]
        tp, fp, fn, tn = r['tp'], r['fp'], r['fn'], r['tn']
        total = tp + fp + fn + tn
        acc = (tp + tn) / total * 100 if total else 0
        errs = fp + fn
        print(f"    {sz}x{sz}: acc={acc:.4f}%, errors={errs:,}/{total:,}")


if __name__ == '__main__':
    main()
