"""
Robust grid detection with per-image scroll calibration.

Algorithm:
1. Use fixed OX, OY, GX, GY (fitted from clean data)
2. For each image, find pink centroids with generous search windows
3. Use MEDIAN of centroid residuals to estimate scroll offset
   (median is robust to 1-2 outliers from animated selection borders)
4. Apply corrected positions for all cells
5. Measure final accuracy

At runtime, this means: scan once with generous windows, calibrate, then
use precise positions for detection. The 1-2 animated items (selected +
previous) will be outliers but the median of 20+ centroids ignores them.
"""
import json
import os
import numpy as np
from PIL import Image
from multiprocessing import Pool, cpu_count

BASE_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
GT_FILE = "F:/Codes/genshin/yas/scripts/grid_ground_truth.json"

with open(GT_FILE) as f:
    _gt = json.load(f)['items']

_gt_lock = {g['idx']: g['lock'] for g in _gt}
_page_items = {}
for g in _gt:
    i = g['idx']
    page = i // 40
    pos = i % 40
    row, col = pos // 8, pos % 8
    _page_items.setdefault(page, []).append((i, row, col))

# Fixed grid parameters (4K) from clean fit
GX_LOCK = 262.3   # GX + lock_dx
GY_LOCK = 386.8   # GY + lock_dy (without scroll)
OX = 292.8
OY = 350.4


def expected_lock_pos(row, col):
    """Expected lock centroid position WITHOUT scroll offset (4K)."""
    return GX_LOCK + col * OX, GY_LOCK + row * OY


def find_centroids(img_np, items, scan_idx):
    """Find pink centroids for all locked items. Uses generous search window."""
    h, w = img_np.shape[:2]
    centroids = []

    for idx, row, col in items:
        if idx == scan_idx:
            continue
        if not _gt_lock[idx]:
            continue

        ex, ey = expected_lock_pos(row, col)
        # Generous search window: ±40px from expected at 4K
        # This absorbs up to ±40px scroll offset
        lx1 = max(0, int(ex) - 40)
        ly1 = max(0, int(ey) - 40)
        lx2 = min(w, int(ex) + 40)
        ly2 = min(h, int(ey) + 40)

        patch = img_np[ly1:ly2, lx1:lx2, :3]
        r = patch[:,:,0].astype(np.int16)
        g = patch[:,:,1].astype(np.int16)
        b = patch[:,:,2].astype(np.int16)
        mask = (r > 180) & ((r-g) > 60) & ((r-b) > 50) & (b > 70)
        if np.sum(mask) < 10:
            continue

        ys, xs = np.where(mask)
        cx = float(np.mean(xs)) + lx1
        cy = float(np.mean(ys)) + ly1

        # Residual from expected (without scroll)
        res_x = cx - ex
        res_y = cy - ey

        centroids.append({
            'idx': idx, 'row': row, 'col': col,
            'actual_x': cx, 'actual_y': cy,
            'res_x': res_x, 'res_y': res_y,
            'is_prev': idx == scan_idx - 1,
        })

    return centroids


def calibrate_scroll(centroids):
    """
    Estimate per-image scroll offset using robust median.

    The median ignores the 1-2 outlier items (selected/previous with
    animated borders). With typically 20+ locked items per page,
    the median is very robust.

    Returns (offset_x, offset_y) to add to expected positions.
    """
    if len(centroids) < 3:
        # Too few points, use mean
        if centroids:
            return (np.mean([c['res_x'] for c in centroids]),
                    np.mean([c['res_y'] for c in centroids]))
        return 0, 0

    res_x = np.array([c['res_x'] for c in centroids])
    res_y = np.array([c['res_y'] for c in centroids])

    return float(np.median(res_x)), float(np.median(res_y))


def process_image(scan_idx):
    """Full pipeline: find centroids → calibrate → measure error."""
    img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
    if not os.path.exists(img_path):
        return []

    page = scan_idx // 40
    items = _page_items.get(page, [])
    img = np.array(Image.open(img_path))

    # Step 1: Find all centroids
    centroids = find_centroids(img, items, scan_idx)
    if not centroids:
        return []

    # Step 2: Calibrate scroll offset using median
    offset_x, offset_y = calibrate_scroll(centroids)

    # Step 3: Measure error for each centroid after calibration
    results = []
    for c in centroids:
        calibrated_x = c['res_x'] - offset_x
        calibrated_y = c['res_y'] - offset_y
        results.append({
            'idx': c['idx'],
            'scan_idx': scan_idx,
            'row': c['row'], 'col': c['col'],
            'err_x_before': c['res_x'],
            'err_y_before': c['res_y'],
            'err_x_after': calibrated_x,
            'err_y_after': calibrated_y,
            'is_prev': c['is_prev'],
            'offset_x': offset_x,
            'offset_y': offset_y,
            'n_centroids': len(centroids),
        })

    return results


def main():
    total = len(_gt)
    print(f"Robust grid detection across {total} images...")

    all_results = []
    with Pool(min(cpu_count(), 8)) as pool:
        for batch in pool.imap_unordered(process_image, range(total), chunksize=20):
            all_results.extend(batch)

    print(f"Total measurements: {len(all_results):,}")

    # Split: excluding vs including the previous item
    clean = [r for r in all_results if not r['is_prev']]
    prev_only = [r for r in all_results if r['is_prev']]

    print(f"Clean (not prev item): {len(clean):,}")
    print(f"Previous item: {len(prev_only):,}")

    # === BEFORE calibration (raw grid, no scroll) ===
    print(f"\n{'='*65}")
    print(f"  BEFORE per-image calibration (at 1080p)")
    print(f"{'='*65}")
    for label, data in [("All", all_results), ("Excl prev", clean)]:
        ex = np.array([r['err_x_before'] for r in data]) / 2
        ey = np.array([r['err_y_before'] for r in data]) / 2
        ed = np.sqrt(ex**2 + ey**2)
        print(f"  {label:12s}: X std={ex.std():.2f}  Y std={ey.std():.2f}  "
              f"dist p95={np.percentile(ed,95):.2f}  max={ed.max():.2f}")

    # === AFTER calibration (with median scroll offset) ===
    print(f"\n{'='*65}")
    print(f"  AFTER per-image median calibration (at 1080p)")
    print(f"{'='*65}")

    for label, data in [("All items", all_results),
                         ("Excl prev item", clean),
                         ("Prev item only", prev_only)]:
        if not data:
            continue
        ex = np.array([r['err_x_after'] for r in data]) / 2
        ey = np.array([r['err_y_after'] for r in data]) / 2
        ed = np.sqrt(ex**2 + ey**2)
        over1 = np.sum(ed > 1)
        over2 = np.sum(ed > 2)
        print(f"\n  {label} (n={len(data):,}):")
        print(f"    X: mean={ex.mean():+.3f}  std={ex.std():.3f}  "
              f"p95={np.percentile(np.abs(ex),95):.3f}  max={np.max(np.abs(ex)):.3f}")
        print(f"    Y: mean={ey.mean():+.3f}  std={ey.std():.3f}  "
              f"p95={np.percentile(np.abs(ey),95):.3f}  max={np.max(np.abs(ey)):.3f}")
        print(f"    Dist: mean={ed.mean():.3f}  p95={np.percentile(ed,95):.3f}  max={ed.max():.3f}")
        print(f"    >1px: {over1:,}/{len(data):,} ({over1/len(data)*100:.2f}%)")
        print(f"    >2px: {over2:,}/{len(data):,} ({over2/len(data)*100:.2f}%)")

    # Per-row after calibration
    print(f"\n  Per-row (excl prev, at 1080p):")
    for row in range(5):
        data = [r for r in clean if r['row'] == row]
        if not data:
            continue
        ex = np.array([r['err_x_after'] for r in data]) / 2
        ey = np.array([r['err_y_after'] for r in data]) / 2
        ed = np.sqrt(ex**2 + ey**2)
        print(f"    Row {row} (n={len(data):,}): "
              f"X std={ex.std():.3f}  Y std={ey.std():.3f}  "
              f"dist p95={np.percentile(ed,95):.3f}  max={ed.max():.3f}")

    # How many centroids per image (affects calibration quality)
    n_per_img = {}
    for r in all_results:
        n_per_img[r['scan_idx']] = r['n_centroids']
    n_vals = list(n_per_img.values())
    print(f"\n  Centroids per image: min={min(n_vals)}  "
          f"p5={np.percentile(n_vals,5):.0f}  "
          f"median={np.median(n_vals):.0f}  max={max(n_vals)}")

    # Error vs number of centroids
    print(f"\n  Error vs calibration point count:")
    for threshold in [5, 10, 15, 20]:
        data = [r for r in clean if r['n_centroids'] >= threshold]
        if not data:
            continue
        ed = np.sqrt(np.array([r['err_x_after'] for r in data])**2 +
                     np.array([r['err_y_after'] for r in data])**2) / 2
        print(f"    n>={threshold:2d} ({len(data):,} pts): "
              f"dist p95={np.percentile(ed,95):.3f}  max={ed.max():.3f}")

    # === What does the previous item's error look like? ===
    if prev_only:
        print(f"\n  Previous item (animated border) error distribution:")
        ex = np.array([r['err_x_after'] for r in prev_only]) / 2
        ey = np.array([r['err_y_after'] for r in prev_only]) / 2
        ed = np.sqrt(ex**2 + ey**2)
        print(f"    dist: mean={ed.mean():.2f}  p95={np.percentile(ed,95):.2f}  max={ed.max():.2f}")
        print(f"    X bias: {ex.mean():+.3f}±{ex.std():.3f}")
        print(f"    Y bias: {ey.mean():+.3f}±{ey.std():.3f}")


if __name__ == "__main__":
    main()
