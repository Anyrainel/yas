"""
Analyze whether the currently-selected item biases grid position measurements.

Theory: The selected item has a thick golden border that expands its visual size,
pushing neighboring items slightly. The scan_idx IS the selected item. The previously
selected item (scan_idx-1) may also have a partially-expanded border (animation).

Check: Do centroids near the selected item show larger errors than distant ones?
Does excluding scan_idx's row/col neighbors improve accuracy?
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

GX, GY = 360, 506
NEW_OX = fit['fit_4k']['OX']
NEW_OY = fit['fit_4k']['OY']
LOCK_DX, LOCK_DY = -97.3, -118.5


def process_image(scan_idx):
    img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
    if not os.path.exists(img_path):
        return []

    page = scan_idx // 40
    items = _page_items.get(page, [])
    img = np.array(Image.open(img_path))
    h, w = img.shape[:2]
    scroll = page_offsets.get(page, 0)

    # Selected item's grid position
    sel_pos = scan_idx % 40
    sel_row, sel_col = sel_pos // 8, sel_pos % 8
    # Previous item (scan_idx - 1) might also be expanded
    prev_idx = scan_idx - 1
    if prev_idx >= 0 and prev_idx // 40 == page:
        prev_pos = prev_idx % 40
        prev_row, prev_col = prev_pos // 8, prev_pos % 8
    else:
        prev_row, prev_col = -1, -1

    results = []
    for idx, row, col in items:
        if idx == scan_idx or not _gt_lock[idx]:
            continue

        cx = GX + col * NEW_OX
        cy = GY + row * NEW_OY + scroll
        lx1 = max(0, int(cx) - 130)
        ly1 = max(0, int(cy) - 165)
        lx2 = min(w, int(cx) - 55)
        ly2 = min(h, int(cy) - 55)
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

        pred_x = cx + LOCK_DX
        pred_y = cy + LOCK_DY
        err_x = actual_x - pred_x
        err_y = actual_y - pred_y

        # Distance to selected item in grid coordinates
        row_dist_sel = abs(row - sel_row)
        col_dist_sel = abs(col - sel_col)
        # Distance to previous item
        row_dist_prev = abs(row - prev_row) if prev_row >= 0 else 99
        col_dist_prev = abs(col - prev_col) if prev_col >= 0 else 99

        # Is this item adjacent to selected or previous?
        adj_sel = row_dist_sel <= 1 and col_dist_sel <= 1
        adj_prev = row_dist_prev <= 1 and col_dist_prev <= 1
        same_row_sel = row == sel_row
        same_col_sel = col == sel_col
        same_row_prev = row == prev_row
        same_col_prev = col == prev_col

        # Relative position to selected item
        rel_row = row - sel_row
        rel_col = col - sel_col

        results.append({
            'idx': idx, 'scan_idx': scan_idx,
            'row': row, 'col': col,
            'err_x': err_x, 'err_y': err_y,
            'row_dist_sel': row_dist_sel,
            'col_dist_sel': col_dist_sel,
            'adj_sel': adj_sel,
            'adj_prev': adj_prev,
            'same_row_sel': same_row_sel,
            'same_col_sel': same_col_sel,
            'rel_row': rel_row,
            'rel_col': rel_col,
        })

    return results


def main():
    total = len(_gt)
    print(f"Analyzing selection bias across {total} images...")

    all_results = []
    with Pool(min(cpu_count(), 8)) as pool:
        for batch in pool.imap_unordered(process_image, range(total), chunksize=20):
            all_results.extend(batch)

    print(f"Total measurements: {len(all_results):,}")

    # Split by proximity to selected item
    adj = [r for r in all_results if r['adj_sel']]
    not_adj = [r for r in all_results if not r['adj_sel']]
    adj_or_prev = [r for r in all_results if r['adj_sel'] or r['adj_prev']]
    far = [r for r in all_results if not r['adj_sel'] and not r['adj_prev']]

    print(f"\n{'='*65}")
    print(f"  ERROR BY PROXIMITY TO SELECTED ITEM (at 1080p)")
    print(f"{'='*65}")

    for label, data in [
        ("Adjacent to selected (±1 row/col)", adj),
        ("NOT adjacent to selected", not_adj),
        ("Adjacent to selected OR previous", adj_or_prev),
        ("Far from both sel and prev", far),
    ]:
        if not data:
            continue
        ex = np.array([r['err_x'] for r in data]) / 2
        ey = np.array([r['err_y'] for r in data]) / 2
        ed = np.sqrt(ex**2 + ey**2)
        print(f"\n  {label} (n={len(data):,}):")
        print(f"    X: mean={ex.mean():+.2f}  std={ex.std():.2f}  p95={np.percentile(np.abs(ex),95):.2f}")
        print(f"    Y: mean={ey.mean():+.2f}  std={ey.std():.2f}  p95={np.percentile(np.abs(ey),95):.2f}")
        print(f"    Dist: mean={ed.mean():.2f}  p95={np.percentile(ed,95):.2f}  max={ed.max():.2f}")
        over2 = np.sum(ed > 2)
        print(f"    >2px: {over2}/{len(data)} ({over2/len(data)*100:.2f}%)")

    # Break down by relative position to selected item
    print(f"\n{'='*65}")
    print(f"  ERROR BY RELATIVE POSITION TO SELECTED (at 1080p)")
    print(f"{'='*65}")

    print(f"\n  Y error by row offset from selected:")
    for rel_row in range(-4, 5):
        data = [r for r in all_results if r['rel_row'] == rel_row]
        if not data:
            continue
        ey = np.array([r['err_y'] for r in data]) / 2
        print(f"    rel_row={rel_row:+d} (n={len(data):5,}):  "
              f"Y mean={ey.mean():+.2f}  std={ey.std():.2f}  "
              f"p95={np.percentile(np.abs(ey),95):.2f}  max_abs={np.max(np.abs(ey)):.2f}")

    print(f"\n  X error by col offset from selected:")
    for rel_col in range(-7, 8):
        data = [r for r in all_results if r['rel_col'] == rel_col]
        if not data:
            continue
        ex = np.array([r['err_x'] for r in data]) / 2
        if len(data) > 100:
            print(f"    rel_col={rel_col:+d} (n={len(data):5,}):  "
                  f"X mean={ex.mean():+.2f}  std={ex.std():.2f}  "
                  f"p95={np.percentile(np.abs(ex),95):.2f}")

    # Same row as selected: does the selected item push neighbors?
    print(f"\n  Same row as selected, by column distance:")
    for cdist in range(8):
        data = [r for r in all_results if r['same_row_sel'] and r['col_dist_sel'] == cdist]
        if len(data) < 50:
            continue
        ex = np.array([r['err_x'] for r in data]) / 2
        ey = np.array([r['err_y'] for r in data]) / 2
        print(f"    col_dist={cdist} (n={len(data):,}):  "
              f"X={ex.mean():+.2f}±{ex.std():.2f}  Y={ey.mean():+.2f}±{ey.std():.2f}")

    # Same column as selected: does it push vertically?
    print(f"\n  Same col as selected, by row distance:")
    for rdist in range(5):
        data = [r for r in all_results if r['same_col_sel'] and r['row_dist_sel'] == rdist]
        if len(data) < 50:
            continue
        ex = np.array([r['err_x'] for r in data]) / 2
        ey = np.array([r['err_y'] for r in data]) / 2
        print(f"    row_dist={rdist} (n={len(data):,}):  "
              f"X={ex.mean():+.2f}±{ex.std():.2f}  Y={ey.mean():+.2f}±{ey.std():.2f}")

    # === REFIT with outlier exclusion ===
    print(f"\n{'='*65}")
    print(f"  REFIT: Excluding items adjacent to selected/previous")
    print(f"{'='*65}")

    # Use only far items to recompute per-page scroll offsets
    from collections import defaultdict
    page_row_data = defaultdict(list)
    for r in all_results:
        if r['adj_sel'] or r['adj_prev']:
            continue
        page_row_data[r['scan_idx']].append(r)

    # Per-page offset from far items only
    page_ey = defaultdict(list)
    for r in far:
        page = r['scan_idx'] // 40
        page_ey[page].append(r['err_y'])

    new_page_offsets = {}
    for page, eys in sorted(page_ey.items()):
        correction = np.mean(eys)
        new_page_offsets[page] = page_offsets.get(page, 0) + correction

    # Re-measure error with corrected offsets
    corrected_errs = []
    for r in far:
        page = r['scan_idx'] // 40
        correction = np.mean(page_ey[page])
        corrected_y = r['err_y'] - correction
        corrected_errs.append({
            'err_x': r['err_x'],
            'err_y_corrected': corrected_y,
        })

    if corrected_errs:
        ex = np.array([r['err_x'] for r in corrected_errs]) / 2
        ey = np.array([r['err_y_corrected'] for r in corrected_errs]) / 2
        ed = np.sqrt(ex**2 + ey**2)
        print(f"\n  Far items with re-centered page offsets (n={len(corrected_errs):,}):")
        print(f"    X: std={ex.std():.2f}  p95={np.percentile(np.abs(ex),95):.2f}")
        print(f"    Y: std={ey.std():.2f}  p95={np.percentile(np.abs(ey),95):.2f}")
        print(f"    Dist: p95={np.percentile(ed,95):.2f}  max={ed.max():.2f}")
        over2 = np.sum(ed > 2)
        print(f"    >2px: {over2}/{len(corrected_errs)} ({over2/len(corrected_errs)*100:.2f}%)")


if __name__ == "__main__":
    main()
