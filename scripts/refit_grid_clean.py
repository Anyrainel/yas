"""
Refit grid parameters excluding the selected item (scan_idx) and previous item
(scan_idx-1) from centroid measurements, since their borders are animated.

Also: the Y-error pattern (items above selected shift negative, below shift positive)
suggests the selected item's expansion displaces the grid slightly. We should
investigate whether this is a real displacement or a measurement artifact.

For the refit: skip idx == scan_idx (already done) AND idx == scan_idx-1.
Then refit OX, OY, and per-page scroll offsets.
"""
import json
import os
import numpy as np
from PIL import Image
from multiprocessing import Pool, cpu_count
from collections import defaultdict

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

GX, GY = 360, 506


def process_image(scan_idx):
    """Extract pink centroids, excluding selected and previous items."""
    img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
    if not os.path.exists(img_path):
        return []

    page = scan_idx // 40
    items = _page_items.get(page, [])
    img = np.array(Image.open(img_path))
    h, w = img.shape[:2]

    # Items to skip: selected (scan_idx) and previous (scan_idx-1)
    skip = {scan_idx, scan_idx - 1}

    results = []
    for idx, row, col in items:
        if idx in skip or not _gt_lock[idx]:
            continue

        # Use a generous search window
        cx_approx = GX + col * 292
        cy_approx = GY + row * 349
        lx1 = max(0, cx_approx - 150)
        ly1 = max(0, cy_approx - 185)
        lx2 = min(w, cx_approx - 35)
        ly2 = min(h, cy_approx - 35)
        patch = img[ly1:ly2, lx1:lx2, :3]
        r = patch[:,:,0].astype(np.int16)
        g = patch[:,:,1].astype(np.int16)
        b = patch[:,:,2].astype(np.int16)
        mask = (r > 180) & ((r-g) > 60) & ((r-b) > 50) & (b > 70)
        if np.sum(mask) < 10:
            continue
        ys, xs = np.where(mask)
        cx = float(np.mean(xs)) + lx1
        cy = float(np.mean(ys)) + ly1

        results.append((scan_idx, idx, row, col, page, cx, cy))

    return results


def main():
    total = len(_gt)
    print(f"Collecting clean centroids (excluding selected + previous)...")

    all_data = []
    with Pool(min(cpu_count(), 8)) as pool:
        for batch in pool.imap_unordered(process_image, range(total), chunksize=20):
            all_data.extend(batch)

    print(f"Clean centroids: {len(all_data):,}")

    # Convert to arrays
    scan_idxs = np.array([d[0] for d in all_data])
    rows = np.array([d[2] for d in all_data])
    cols = np.array([d[3] for d in all_data])
    pages = np.array([d[4] for d in all_data])
    cx = np.array([d[5] for d in all_data])
    cy = np.array([d[6] for d in all_data])

    # ==========================================
    # Step 1: Fit OX, lock_dx from X data (no page offset needed)
    # cx = GX + col * OX + lock_dx
    # cx = (GX + lock_dx) + col * OX
    # ==========================================
    A_x = np.column_stack([np.ones(len(cols)), cols])
    result_x = np.linalg.lstsq(A_x, cx, rcond=None)
    gx_plus_dx, ox = result_x[0]
    print(f"\nX fit: GX+lock_dx = {gx_plus_dx:.2f}, OX = {ox:.2f}")

    # ==========================================
    # Step 2: Fit OY, lock_dy, per-page offsets from Y data
    # cy = GY + row * OY + lock_dy + page_offset[page]
    # cy = (GY + lock_dy) + row * OY + page_offset[page]
    # ==========================================
    unique_pages = sorted(set(pages))
    n_pages = len(unique_pages)
    page_to_idx = {p: i for i, p in enumerate(unique_pages)}

    # Design matrix: [1, row, page_indicator_columns...]
    # But page 0 is reference (offset=0)
    n = len(cy)
    A_y = np.zeros((n, 2 + n_pages - 1))
    A_y[:, 0] = 1  # intercept (GY + lock_dy)
    A_y[:, 1] = rows  # OY coefficient

    for i, p in enumerate(pages):
        pidx = page_to_idx[p]
        if pidx > 0:  # page 0 is reference
            A_y[i, 2 + pidx - 1] = 1

    result_y = np.linalg.lstsq(A_y, cy, rcond=None)
    coeffs = result_y[0]
    gy_plus_dy = coeffs[0]
    oy = coeffs[1]
    page_scroll = {unique_pages[0]: 0.0}
    for i, p in enumerate(unique_pages[1:]):
        page_scroll[p] = coeffs[2 + i]

    print(f"Y fit: GY+lock_dy = {gy_plus_dy:.2f}, OY = {oy:.2f}")
    print(f"Pages fitted: {n_pages}")

    # ==========================================
    # Step 3: Compute residuals
    # ==========================================
    pred_x = gx_plus_dx + cols * ox
    pred_y = gy_plus_dy + rows * oy + np.array([page_scroll.get(p, 0) for p in pages])
    res_x = cx - pred_x
    res_y = cy - pred_y
    res_dist = np.sqrt(res_x**2 + res_y**2)

    print(f"\n{'='*65}")
    print(f"  CLEAN FIT RESIDUALS (at 4K)")
    print(f"{'='*65}")
    print(f"  X: mean={res_x.mean():+.2f}  std={res_x.std():.2f}  "
          f"p95={np.percentile(np.abs(res_x),95):.2f}  max={np.max(np.abs(res_x)):.2f}")
    print(f"  Y: mean={res_y.mean():+.2f}  std={res_y.std():.2f}  "
          f"p95={np.percentile(np.abs(res_y),95):.2f}  max={np.max(np.abs(res_y)):.2f}")
    print(f"  Dist: p95={np.percentile(res_dist,95):.2f}  max={res_dist.max():.2f}")

    print(f"\n{'='*65}")
    print(f"  AT 1080p")
    print(f"{'='*65}")
    print(f"  X: std={res_x.std()/2:.2f}px  p95={np.percentile(np.abs(res_x),95)/2:.2f}px")
    print(f"  Y: std={res_y.std()/2:.2f}px  p95={np.percentile(np.abs(res_y),95)/2:.2f}px")
    print(f"  Dist: p95={np.percentile(res_dist,95)/2:.2f}px  max={res_dist.max()/2:.2f}px")
    over2 = np.sum(res_dist/2 > 2)
    over1 = np.sum(res_dist/2 > 1)
    print(f"  >1px: {over1:,}/{len(res_dist):,} ({over1/len(res_dist)*100:.2f}%)")
    print(f"  >2px: {over2:,}/{len(res_dist):,} ({over2/len(res_dist)*100:.2f}%)")

    # Per-row breakdown
    print(f"\n  Per-row (at 1080p):")
    for row in range(5):
        mask = rows == row
        if np.sum(mask) == 0:
            continue
        rx = res_x[mask] / 2
        ry = res_y[mask] / 2
        rd = res_dist[mask] / 2
        print(f"    Row {row}: X std={rx.std():.2f}  Y std={ry.std():.2f}  "
              f"dist p95={np.percentile(rd,95):.2f}  max={rd.max():.2f}")

    # ==========================================
    # Step 4: Scroll offset pattern
    # ==========================================
    print(f"\n{'='*65}")
    print(f"  PER-PAGE SCROLL OFFSETS (4K)")
    print(f"{'='*65}")
    for mod3 in [0, 1, 2]:
        pages_mod = [p for p in unique_pages if p % 3 == mod3]
        offsets = [page_scroll[p] for p in pages_mod]
        print(f"  mod3={mod3}: mean={np.mean(offsets):+.1f}  std={np.std(offsets):.1f}  "
              f"range=[{min(offsets):+.1f}, {max(offsets):+.1f}]")

    # Save results
    results = {
        'description': 'Clean fit excluding selected and previous items',
        'n_centroids': len(all_data),
        'fit_4k': {
            'OX': round(ox, 3),
            'OY': round(oy, 3),
            'GX_plus_lock_dx': round(gx_plus_dx, 3),
            'GY_plus_lock_dy': round(gy_plus_dy, 3),
        },
        'fit_1080p': {
            'OX': round(ox/2, 3),
            'OY': round(oy/2, 3),
            'GX_plus_lock_dx': round(gx_plus_dx/2, 3),
            'GY_plus_lock_dy': round(gy_plus_dy/2, 3),
        },
        'residuals_1080p': {
            'x_std': round(res_x.std()/2, 3),
            'y_std': round(res_y.std()/2, 3),
            'dist_p95': round(np.percentile(res_dist, 95)/2, 3),
            'dist_max': round(res_dist.max()/2, 3),
            'pct_over_2px': round(over2/len(res_dist)*100, 3),
        },
        'per_page_y_scroll_offsets': {str(p): round(v, 2) for p, v in sorted(page_scroll.items())},
    }

    out_path = "F:/Codes/genshin/yas/scripts/grid_fit_clean.json"
    with open(out_path, 'w') as f:
        json.dump(results, f, indent=2)
    print(f"\nSaved to {out_path}")


if __name__ == "__main__":
    main()
