"""
Measure final grid error after applying corrected OY + per-page scroll offsets.

Uses pink lock centroids as ground truth positions.
Reports error at both 4K and 1080p resolution.
"""
import json
import os
import numpy as np
from PIL import Image
from multiprocessing import Pool, cpu_count

BASE_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
GT_FILE = "F:/Codes/genshin/yas/scripts/grid_ground_truth.json"
FIT_FILE = "F:/Codes/genshin/yas/scripts/grid_fit_results.json"

with open(FIT_FILE) as f:
    fit = json.load(f)
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

page_offsets = {int(k): v for k, v in fit['per_page_y_scroll_offsets'].items()}

# Parameters at 4K
GX = 360
GY = 506
OLD_OX, OLD_OY = 290, 332
NEW_OX = fit['fit_4k']['OX']   # 292.5
NEW_OY = fit['fit_4k']['OY']   # 349.4
LOCK_DX = -97.3  # from fit
LOCK_DY = -118.5


def process_image(scan_idx):
    img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
    if not os.path.exists(img_path):
        return []

    page = scan_idx // 40
    items = _page_items.get(page, [])
    img = np.array(Image.open(img_path))
    h, w = img.shape[:2]
    scroll = page_offsets.get(page, 0)

    results = []
    for idx, row, col in items:
        if idx == scan_idx or not _gt_lock[idx]:
            continue

        # Find actual pink centroid
        cx_new = GX + col * NEW_OX
        cy_new = GY + row * NEW_OY + scroll
        lx1 = max(0, int(cx_new) - 130)
        ly1 = max(0, int(cy_new) - 165)
        lx2 = min(w, int(cx_new) - 55)
        ly2 = min(h, int(cy_new) - 55)
        patch = img[ly1:ly2, lx1:lx2, :3]
        r = patch[:,:,0].astype(np.int16)
        g = patch[:,:,1].astype(np.int16)
        b = patch[:,:,2].astype(np.int16)
        mask = (r > 180) & ((r-g) > 60) & ((r-b) > 50) & (b > 70)
        if np.sum(mask) < 10:
            continue
        ys, xs = np.where(mask)
        actual_x = float(np.mean(xs)) + lx1
        actual_y = float(np.mean(ys)) + ly1

        # Predicted lock position (NEW grid)
        pred_x_new = cx_new + LOCK_DX
        pred_y_new = cy_new + LOCK_DY

        # Predicted lock position (OLD grid)
        cx_old = GX + col * OLD_OX
        cy_old = GY + row * OLD_OY
        pred_x_old = cx_old + (-89)   # old lock offset
        pred_y_old = cy_old + (-112)

        results.append({
            'idx': idx, 'row': row, 'col': col, 'page': page,
            'old_err_x': actual_x - pred_x_old,
            'old_err_y': actual_y - pred_y_old,
            'new_err_x': actual_x - pred_x_new,
            'new_err_y': actual_y - pred_y_new,
        })

    return results


def main():
    total = len(_gt)
    print(f"Measuring grid error across {total} images...")

    all_results = []
    with Pool(min(cpu_count(), 8)) as pool:
        for batch in pool.imap_unordered(process_image, range(total), chunksize=20):
            all_results.extend(batch)

    print(f"Total measurements: {len(all_results):,}")

    old_ex = np.array([r['old_err_x'] for r in all_results])
    old_ey = np.array([r['old_err_y'] for r in all_results])
    new_ex = np.array([r['new_err_x'] for r in all_results])
    new_ey = np.array([r['new_err_y'] for r in all_results])

    old_dist = np.sqrt(old_ex**2 + old_ey**2)
    new_dist = np.sqrt(new_ex**2 + new_ey**2)

    print(f"\n{'='*65}")
    print(f"  GRID ERROR COMPARISON (at 4K resolution)")
    print(f"{'='*65}")
    print(f"\n  OLD grid (OY=332, no scroll offset):")
    print(f"    X error: mean={old_ex.mean():+.1f}  std={np.std(old_ex):.1f}  "
          f"p95={np.percentile(np.abs(old_ex),95):.1f}  max={np.max(np.abs(old_ex)):.1f}")
    print(f"    Y error: mean={old_ey.mean():+.1f}  std={np.std(old_ey):.1f}  "
          f"p95={np.percentile(np.abs(old_ey),95):.1f}  max={np.max(np.abs(old_ey)):.1f}")
    print(f"    Distance: mean={old_dist.mean():.1f}  p95={np.percentile(old_dist,95):.1f}  "
          f"max={old_dist.max():.1f}")

    print(f"\n  NEW grid (OY=349.4, with per-page scroll):")
    print(f"    X error: mean={new_ex.mean():+.1f}  std={np.std(new_ex):.1f}  "
          f"p95={np.percentile(np.abs(new_ex),95):.1f}  max={np.max(np.abs(new_ex)):.1f}")
    print(f"    Y error: mean={new_ey.mean():+.1f}  std={np.std(new_ey):.1f}  "
          f"p95={np.percentile(np.abs(new_ey),95):.1f}  max={np.max(np.abs(new_ey)):.1f}")
    print(f"    Distance: mean={new_dist.mean():.1f}  p95={np.percentile(new_dist,95):.1f}  "
          f"max={new_dist.max():.1f}")

    print(f"\n{'='*65}")
    print(f"  AT 1080p RESOLUTION (divide by 2)")
    print(f"{'='*65}")
    print(f"\n  OLD grid:")
    print(f"    X: std={np.std(old_ex)/2:.1f}px  p95={np.percentile(np.abs(old_ex),95)/2:.1f}px")
    print(f"    Y: std={np.std(old_ey)/2:.1f}px  p95={np.percentile(np.abs(old_ey),95)/2:.1f}px")
    print(f"    Dist p95={np.percentile(old_dist,95)/2:.1f}px  max={old_dist.max()/2:.1f}px")

    print(f"\n  NEW grid:")
    print(f"    X: std={np.std(new_ex)/2:.1f}px  p95={np.percentile(np.abs(new_ex),95)/2:.1f}px")
    print(f"    Y: std={np.std(new_ey)/2:.1f}px  p95={np.percentile(np.abs(new_ey),95)/2:.1f}px")
    print(f"    Dist p95={np.percentile(new_dist,95)/2:.1f}px  max={new_dist.max()/2:.1f}px")

    # Per-row breakdown
    print(f"\n{'='*65}")
    print(f"  PER-ROW ERROR (NEW grid, at 1080p)")
    print(f"{'='*65}")
    for row in range(5):
        mask = np.array([r['row'] == row for r in all_results])
        if np.sum(mask) == 0:
            continue
        rx = new_ex[mask] / 2
        ry = new_ey[mask] / 2
        rd = new_dist[mask] / 2
        print(f"  Row {row} (n={np.sum(mask):,}):  "
              f"X std={rx.std():.2f}  Y std={ry.std():.2f}  "
              f"dist p95={np.percentile(rd,95):.2f}  max={rd.max():.2f}")

    # Check: how many exceed 2px at 1080p?
    over_2 = np.sum(new_dist / 2 > 2)
    over_4 = np.sum(new_dist / 2 > 4)
    print(f"\n  Measurements exceeding 2px at 1080p: {over_2:,}/{len(all_results):,} "
          f"({over_2/len(all_results)*100:.2f}%)")
    print(f"  Measurements exceeding 4px at 1080p: {over_4:,}/{len(all_results):,} "
          f"({over_4/len(all_results)*100:.2f}%)")

    # Is the remaining error from centroid measurement noise or from grid imprecision?
    # Check within-page consistency
    from collections import defaultdict
    page_row_errors = defaultdict(list)
    for r in all_results:
        page_row_errors[(r['page'], r['row'])].append((r['new_err_x'], r['new_err_y']))

    within_stds_x = []
    within_stds_y = []
    for key, errs in page_row_errors.items():
        if len(errs) >= 5:
            ex = [e[0] for e in errs]
            ey = [e[1] for e in errs]
            within_stds_x.append(np.std(ex))
            within_stds_y.append(np.std(ey))

    if within_stds_x:
        print(f"\n  Within-page-row consistency (std at 1080p):")
        print(f"    X: mean={np.mean(within_stds_x)/2:.2f}px  max={max(within_stds_x)/2:.2f}px")
        print(f"    Y: mean={np.mean(within_stds_y)/2:.2f}px  max={max(within_stds_y)/2:.2f}px")
        print(f"    (This is the irreducible centroid measurement noise)")


if __name__ == "__main__":
    main()
