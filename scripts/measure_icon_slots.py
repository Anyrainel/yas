"""
Measure icon slot positions relative to the card center, using pink lock centroids
as anchor points.

For each card, the top-left corner has up to 3 icon slots stacked vertically:
  Slot 1 (topmost): lock icon (if locked)
  Slot 2: astral mark (if locked+astral)
  Slot 3: elixir mark (pushed down by lock/astral above)

We measure:
1. Lock centroid relative to card center (already known, refined here)
2. Astral star centroid relative to lock centroid (vertical spacing)
3. Elixir icon centroid relative to lock/astral centroid
4. Card edge positions to compute slot offsets from card top-left
"""

import json
import os
import sys
import numpy as np
from PIL import Image
from multiprocessing import Pool, cpu_count
from collections import defaultdict

BASE_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
SCAN_FILE = "F:/Codes/genshin/yas/target/release/good_export_2026-03-29_01-51-46.json"
GT_FILE = "F:/Codes/genshin/yas/scripts/grid_ground_truth.json"

# Grid params at 4K (from refit_grid_clean.py results)
GX_LOCK = 262.3   # GX + lock_dx at 4K
GY_LOCK = 386.8   # GY + lock_dy at 4K (page 0 reference)
OX = 292.8         # horizontal cell spacing
OY = 350.4         # vertical cell spacing
GX_CENTER = 359.6  # cell center X
GY_CENTER = 505.3  # cell center Y

# Load ground truth and scan data
with open(GT_FILE) as f:
    _gt_items = json.load(f)['items']
_gt = {g['idx']: g for g in _gt_items}

with open(SCAN_FILE) as f:
    _arts = json.load(f)['artifacts']

# Build page -> items mapping
_page_items = {}
for g in _gt_items:
    i = g['idx']
    page = i // 40
    pos = i % 40
    row, col = pos // 8, pos % 8
    _page_items.setdefault(page, []).append((i, row, col))


def is_pink(r, g, b):
    return r > 180 and (r - g) > 60 and (r - b) > 50 and b > 70


def is_yellow(r, g, b):
    return r > 220 and g > 170 and b < 80


def is_elixir_purple(r, g, b):
    """Detect elixir icon purple/blue pixels."""
    return b > 150 and (b - r) > 30 and (b - g) > 30


def find_centroid(img, x1, y1, x2, y2, color_fn):
    """Find centroid of pixels matching color_fn in region. Returns (cx, cy, count) or None."""
    x1, y1 = max(0, x1), max(0, y1)
    x2, y2 = min(img.shape[1], x2), min(img.shape[0], y2)
    patch = img[y1:y2, x1:x2, :3]
    r = patch[:, :, 0].astype(np.int16)
    g = patch[:, :, 1].astype(np.int16)
    b = patch[:, :, 2].astype(np.int16)

    if color_fn == 'pink':
        mask = (r > 180) & ((r - g) > 60) & ((r - b) > 50) & (b > 70)
    elif color_fn == 'yellow':
        mask = (r > 220) & (g > 170) & (b < 80)
    elif color_fn == 'purple':
        mask = (b > 150) & ((b - r) > 30) & ((b - g) > 30)
    else:
        return None

    count = int(np.sum(mask))
    if count < 5:
        return None

    ys, xs = np.where(mask)
    cx = float(np.mean(xs)) + x1
    cy = float(np.mean(ys)) + y1
    return cx, cy, count


def process_image(scan_idx):
    """For one full.png, find lock/astral/elixir centroids for all grid items."""
    img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
    if not os.path.exists(img_path):
        return []

    page = scan_idx // 40
    items = _page_items.get(page, [])
    img = np.array(Image.open(img_path))
    h, w = img.shape[:2]

    # Skip selected item and previous (animated borders)
    skip = {scan_idx, scan_idx - 1}

    # First pass: find all pink lock centroids for scroll calibration
    lock_residuals_y = []
    lock_data = {}

    for idx, row, col in items:
        if idx in skip or not _gt[idx]['lock']:
            continue

        cx_exp = GX_LOCK + col * OX
        cy_exp = GY_LOCK + row * OY

        lx1 = max(0, int(cx_exp - 60))
        ly1 = max(0, int(cy_exp - 60))
        lx2 = min(w, int(cx_exp + 60))
        ly2 = min(h, int(cy_exp + 60))

        result = find_centroid(img, lx1, ly1, lx2, ly2, 'pink')
        if result and result[2] >= 10:
            cx_found, cy_found, cnt = result
            lock_data[(idx, row, col)] = (cx_found, cy_found, cnt)
            lock_residuals_y.append(cy_found - cy_exp)

    if not lock_residuals_y:
        return []

    # Per-image scroll offset (Y only, X is stable)
    scroll_offset_y = float(np.median(lock_residuals_y))

    results = []

    for (idx, row, col), (lock_cx, lock_cy, lock_cnt) in lock_data.items():
        art = _arts[idx]
        has_astral = _gt[idx].get('astralMark', False)
        has_elixir = art.get('elixirCrafted', False)

        record = {
            'scan_idx': scan_idx,
            'idx': idx,
            'row': row,
            'col': col,
            'lock_cx': lock_cx,
            'lock_cy': lock_cy,
            'lock_cnt': lock_cnt,
            'scroll_offset_y': scroll_offset_y,
            'has_astral': has_astral,
            'has_elixir': has_elixir,
        }

        # Astral: search below the lock icon
        if has_astral:
            # Astral star should be directly below lock, ~20-50px down
            ax1 = max(0, int(lock_cx - 40))
            ay1 = max(0, int(lock_cy + 10))
            ax2 = min(w, int(lock_cx + 40))
            ay2 = min(h, int(lock_cy + 80))
            astral_result = find_centroid(img, ax1, ay1, ax2, ay2, 'yellow')
            if astral_result:
                record['astral_cx'] = astral_result[0]
                record['astral_cy'] = astral_result[1]
                record['astral_cnt'] = astral_result[2]
                record['astral_dx'] = astral_result[0] - lock_cx
                record['astral_dy'] = astral_result[1] - lock_cy

        # Elixir: search below lock (and below astral if present)
        if has_elixir:
            # If astral present, elixir is below astral; otherwise below lock
            if has_astral and 'astral_cy' in record:
                ref_cy = record['astral_cy']
                search_start = int(ref_cy + 10)
            else:
                ref_cy = lock_cy
                search_start = int(lock_cy + 10)

            ex1 = max(0, int(lock_cx - 40))
            ey1 = max(0, search_start)
            ex2 = min(w, int(lock_cx + 40))
            ey2 = min(h, search_start + 80)
            elixir_result = find_centroid(img, ex1, ey1, ex2, ey2, 'purple')
            if elixir_result:
                record['elixir_cx'] = elixir_result[0]
                record['elixir_cy'] = elixir_result[1]
                record['elixir_cnt'] = elixir_result[2]
                record['elixir_dx_from_lock'] = elixir_result[0] - lock_cx
                record['elixir_dy_from_lock'] = elixir_result[1] - lock_cy

        results.append(record)

    return results


def main():
    total = len(_arts)

    # Sample ~500 images evenly
    sample_size = 500
    step = max(1, total // sample_size)
    sample_indices = list(range(0, total, step))[:sample_size]
    print(f"Sampling {len(sample_indices)} images out of {total}")

    all_records = []
    with Pool(min(cpu_count(), 8)) as pool:
        for batch in pool.imap_unordered(process_image, sample_indices, chunksize=10):
            all_records.extend(batch)

    print(f"Total records collected: {len(all_records)}")

    # =========================================
    # Analysis 1: Lock centroid relative to cell center
    # =========================================
    lock_dx_vals = []
    lock_dy_vals = []
    for r in all_records:
        cx_center = GX_CENTER + r['col'] * OX
        cy_center = GY_CENTER + r['row'] * OY + r['scroll_offset_y']
        lock_dx_vals.append(r['lock_cx'] - cx_center)
        lock_dy_vals.append(r['lock_cy'] - cy_center)

    lock_dx = np.array(lock_dx_vals)
    lock_dy = np.array(lock_dy_vals)

    print("\n" + "=" * 60)
    print("1. LOCK ICON CENTROID relative to CELL CENTER (4K pixels)")
    print("=" * 60)
    print(f"  dx: mean={np.mean(lock_dx):.2f}, std={np.std(lock_dx):.2f}, "
          f"min={np.min(lock_dx):.2f}, max={np.max(lock_dx):.2f}")
    print(f"  dy: mean={np.mean(lock_dy):.2f}, std={np.std(lock_dy):.2f}, "
          f"min={np.min(lock_dy):.2f}, max={np.max(lock_dy):.2f}")
    print(f"  (At 1080p: dx={np.mean(lock_dx)/2:.1f}, dy={np.mean(lock_dy)/2:.1f})")

    # =========================================
    # Analysis 2: Astral centroid relative to lock centroid
    # =========================================
    astral_dx_vals = [r['astral_dx'] for r in all_records if 'astral_dx' in r]
    astral_dy_vals = [r['astral_dy'] for r in all_records if 'astral_dy' in r]

    if astral_dx_vals:
        adx = np.array(astral_dx_vals)
        ady = np.array(astral_dy_vals)
        print("\n" + "=" * 60)
        print("2. ASTRAL MARK CENTROID relative to LOCK CENTROID (4K pixels)")
        print("=" * 60)
        print(f"  N samples: {len(adx)}")
        print(f"  dx: mean={np.mean(adx):.2f}, std={np.std(adx):.2f}, "
              f"min={np.min(adx):.2f}, max={np.max(adx):.2f}")
        print(f"  dy: mean={np.mean(ady):.2f}, std={np.std(ady):.2f}, "
              f"min={np.min(ady):.2f}, max={np.max(ady):.2f}")
        print(f"  (At 1080p: dx={np.mean(adx)/2:.1f}, dy={np.mean(ady)/2:.1f})")

        # Astral centroid relative to cell center
        astral_abs_dx = [r['astral_cx'] - (GX_CENTER + r['col'] * OX) for r in all_records if 'astral_cx' in r]
        astral_abs_dy = [r['astral_cy'] - (GY_CENTER + r['row'] * OY + r['scroll_offset_y']) for r in all_records if 'astral_cy' in r]
        aadx = np.array(astral_abs_dx)
        aady = np.array(astral_abs_dy)
        print(f"\n  Astral relative to CELL CENTER:")
        print(f"  dx: mean={np.mean(aadx):.2f}, std={np.std(aadx):.2f}")
        print(f"  dy: mean={np.mean(aady):.2f}, std={np.std(aady):.2f}")
        print(f"  (At 1080p: dx={np.mean(aadx)/2:.1f}, dy={np.mean(aady)/2:.1f})")
    else:
        print("\n[No astral+locked samples found in sampled images]")

    # =========================================
    # Analysis 3: Elixir centroid
    # =========================================
    elixir_from_lock = [(r['elixir_dx_from_lock'], r['elixir_dy_from_lock']) for r in all_records if 'elixir_dx_from_lock' in r]

    if elixir_from_lock:
        edx = np.array([e[0] for e in elixir_from_lock])
        edy = np.array([e[1] for e in elixir_from_lock])
        print("\n" + "=" * 60)
        print("3. ELIXIR MARK CENTROID relative to LOCK CENTROID (4K pixels)")
        print("=" * 60)
        print(f"  N samples: {len(edx)}")
        print(f"  dx: mean={np.mean(edx):.2f}, std={np.std(edx):.2f}, "
              f"min={np.min(edx):.2f}, max={np.max(edx):.2f}")
        print(f"  dy: mean={np.mean(edy):.2f}, std={np.std(edy):.2f}, "
              f"min={np.min(edy):.2f}, max={np.max(edy):.2f}")
        print(f"  (At 1080p: dx={np.mean(edx)/2:.1f}, dy={np.mean(edy)/2:.1f})")

        # Break down by has_astral
        with_astral = [(r['elixir_dy_from_lock'],) for r in all_records if 'elixir_dy_from_lock' in r and r['has_astral']]
        without_astral = [(r['elixir_dy_from_lock'],) for r in all_records if 'elixir_dy_from_lock' in r and not r['has_astral']]

        if with_astral:
            wa = np.array([e[0] for e in with_astral])
            print(f"\n  Elixir dy from lock (WITH astral above): mean={np.mean(wa):.2f}, N={len(wa)}")
        if without_astral:
            woa = np.array([e[0] for e in without_astral])
            print(f"  Elixir dy from lock (WITHOUT astral): mean={np.mean(woa):.2f}, N={len(woa)}")
    else:
        print("\n[No locked+elixir samples found in sampled images]")

    # =========================================
    # Analysis 4: Card edge detection for absolute slot positions
    # =========================================
    # For a few images, detect the card top edge via luminance gradient
    print("\n" + "=" * 60)
    print("4. CARD EDGE DETECTION (sampling a few items)")
    print("=" * 60)

    card_top_offsets = []
    card_left_offsets = []

    # Sample some records for card edge measurement
    edge_samples = all_records[:min(200, len(all_records))]
    for r in edge_samples:
        scan_idx = r['scan_idx']
        img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
        if not os.path.exists(img_path):
            continue

        row, col = r['row'], r['col']
        cx_center = GX_CENTER + col * OX
        cy_center = GY_CENTER + row * OY + r['scroll_offset_y']

        img = np.array(Image.open(img_path))

        # Detect top edge: scan upward from cell center, look for dark-to-light transition
        # The card has a darker border/gap between cards
        x_probe = int(cx_center)
        y_start = int(cy_center - 100)  # well inside the card

        # Find the top edge by looking for where brightness drops sharply
        # (card interior is lighter, gap between cards is darker)
        prev_brightness = None
        top_edge_y = None
        for y in range(y_start, max(0, int(cy_center - 200)), -1):
            if 0 <= y < img.shape[0] and 0 <= x_probe < img.shape[1]:
                px = img[y, x_probe, :3].astype(float)
                brightness = (px[0] + px[1] + px[2]) / 3
                if prev_brightness is not None and prev_brightness - brightness > 30:
                    top_edge_y = y + 1  # the brighter pixel is the card top
                    break
                prev_brightness = brightness

        if top_edge_y is not None:
            card_top_offsets.append(top_edge_y - cy_center)

        # Detect left edge: scan leftward from cell center
        y_probe = int(cy_center)
        x_start = int(cx_center - 50)  # inside the card

        prev_brightness = None
        left_edge_x = None
        for x in range(x_start, max(0, int(cx_center - 200)), -1):
            if 0 <= y_probe < img.shape[0] and 0 <= x < img.shape[1]:
                px = img[y_probe, x, :3].astype(float)
                brightness = (px[0] + px[1] + px[2]) / 3
                if prev_brightness is not None and prev_brightness - brightness > 30:
                    left_edge_x = x + 1
                    break
                prev_brightness = brightness

        if left_edge_x is not None:
            card_left_offsets.append(left_edge_x - cx_center)

    if card_top_offsets:
        cto = np.array(card_top_offsets)
        print(f"  Card TOP edge offset from cell center (4K):")
        print(f"    mean={np.mean(cto):.2f}, std={np.std(cto):.2f}, "
              f"min={np.min(cto):.2f}, max={np.max(cto):.2f}")
        print(f"    (At 1080p: {np.mean(cto)/2:.1f})")

    if card_left_offsets:
        clo = np.array(card_left_offsets)
        print(f"  Card LEFT edge offset from cell center (4K):")
        print(f"    mean={np.mean(clo):.2f}, std={np.std(clo):.2f}, "
              f"min={np.min(clo):.2f}, max={np.max(clo):.2f}")
        print(f"    (At 1080p: {np.mean(clo)/2:.1f})")

    # =========================================
    # Summary: Slot positions
    # =========================================
    print("\n" + "=" * 60)
    print("5. SUMMARY — SLOT POSITIONS")
    print("=" * 60)

    lock_dx_mean = np.mean(lock_dx)
    lock_dy_mean = np.mean(lock_dy)
    print(f"\n  Lock icon (slot 1) relative to cell center (4K):")
    print(f"    dx={lock_dx_mean:.2f}, dy={lock_dy_mean:.2f}")
    print(f"    (1080p: dx={lock_dx_mean/2:.1f}, dy={lock_dy_mean/2:.1f})")

    if astral_dy_vals:
        astral_spacing = np.mean(astral_dy_vals)
        print(f"\n  Astral mark (slot 2) relative to lock icon:")
        print(f"    dy={astral_spacing:.2f} (4K), {astral_spacing/2:.1f} (1080p)")
        print(f"  Astral mark relative to cell center (4K):")
        print(f"    dx={lock_dx_mean + np.mean(astral_dx_vals):.2f}, dy={lock_dy_mean + astral_spacing:.2f}")

    if elixir_from_lock:
        # Elixir when only lock above (no astral)
        if without_astral:
            elixir_dy_slot2 = np.mean([e[0] for e in without_astral])
            print(f"\n  Elixir mark in slot 2 (lock above, no astral):")
            print(f"    dy from lock={elixir_dy_slot2:.2f} (4K), {elixir_dy_slot2/2:.1f} (1080p)")

    if card_top_offsets and card_left_offsets:
        top_mean = np.mean(card_top_offsets)
        left_mean = np.mean(card_left_offsets)
        print(f"\n  Lock icon relative to card TOP-LEFT corner (4K):")
        print(f"    from_left={lock_dx_mean - left_mean:.2f}, from_top={lock_dy_mean - top_mean:.2f}")
        print(f"    (1080p: from_left={(lock_dx_mean - left_mean)/2:.1f}, from_top={(lock_dy_mean - top_mean)/2:.1f})")

    # =========================================
    # Analysis 6: Pixel counts for each icon type
    # =========================================
    print("\n" + "=" * 60)
    print("6. PIXEL COUNTS per icon type")
    print("=" * 60)

    lock_counts = [r['lock_cnt'] for r in all_records]
    print(f"  Lock pink pixels: mean={np.mean(lock_counts):.1f}, std={np.std(lock_counts):.1f}, "
          f"min={np.min(lock_counts)}, max={np.max(lock_counts)}")

    astral_counts = [r['astral_cnt'] for r in all_records if 'astral_cnt' in r]
    if astral_counts:
        print(f"  Astral yellow pixels: mean={np.mean(astral_counts):.1f}, std={np.std(astral_counts):.1f}, "
              f"min={np.min(astral_counts)}, max={np.max(astral_counts)}")

    elixir_counts = [r['elixir_cnt'] for r in all_records if 'elixir_cnt' in r]
    if elixir_counts:
        print(f"  Elixir purple pixels: mean={np.mean(elixir_counts):.1f}, std={np.std(elixir_counts):.1f}, "
              f"min={np.min(elixir_counts)}, max={np.max(elixir_counts)}")


if __name__ == '__main__':
    main()
