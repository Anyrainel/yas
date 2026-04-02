"""
Region-based lock icon detection analysis.

The lock icon has a dark semi-transparent background.
Instead of single pixels, compute average brightness over a small region
and compare locked vs unlocked cells.

Also try: local contrast (variance), color channel ratios, etc.
"""

import json
import os
import numpy as np
from PIL import Image

BASE_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
SCAN_FILE = "F:/Codes/genshin/yas/target/release/good_export_2026-03-29_01-51-46.json"

SCALE = 2.0
GRID_FIRST_X = int(180.0 * SCALE)
GRID_FIRST_Y = int(253.0 * SCALE)
GRID_OFFSET_X = int(145.0 * SCALE)
GRID_OFFSET_Y = int(166.0 * SCALE)
GRID_COLS = 8
GRID_ROWS = 5

def grid_center(row, col):
    x = GRID_FIRST_X + col * GRID_OFFSET_X
    y = GRID_FIRST_Y + row * GRID_OFFSET_Y
    return x, y

def main():
    with open(SCAN_FILE) as f:
        data = json.load(f)
    arts = data['artifacts']

    # Build page -> items mapping
    pages = {}
    for i, a in enumerate(arts):
        page = i // 40
        pos = i % 40
        row, col = pos // 8, pos % 8
        if page not in pages:
            pages[page] = []
        pages[page].append({
            'idx': i, 'row': row, 'col': col,
            'lock': a.get('lock', False),
        })

    # Strategy: extract a small region around the lock icon position
    # and compute different features

    # From visual inspection, the lock icon center is approximately at
    # dx=-89, dy=-100 from cell center (4K)
    # The dark bg extends ~25px around it
    # Let's try different region sizes and positions

    # Also try: look at a reference region WITHOUT the icon (e.g., upper-right corner)
    # and compare to the lock region. If there's a lock, the lock region will be darker
    # relative to the reference. This normalizes for different card backgrounds.

    # Candidate regions (4K coords, relative to cell center):
    # Lock icon bg: centered around (-89, -100), size 30x30
    # Reference: same Y but opposite X, e.g., (+20, -100), size 30x30

    test_configs = [
        # (name, lock_region, ref_region)
        # Each region is (dx, dy, w, h) relative to cell center
        ("lock_30x30_vs_ref", (-105, -115, 30, 30), (-30, -115, 30, 30)),
        ("lock_20x20_vs_ref", (-99, -110, 20, 20), (-30, -110, 20, 20)),
        ("lock_40x40_vs_ref", (-110, -120, 40, 40), (-20, -120, 40, 40)),
        ("lock_30x30_vs_below", (-105, -115, 30, 30), (-105, -50, 30, 30)),
        ("lock_abs_30x30", (-105, -115, 30, 30), None),
        ("lock_abs_20x20", (-99, -110, 20, 20), None),
        ("lock_abs_50x50", (-114, -125, 50, 50), None),
        # Try the dark bg edge (should be consistently dark)
        ("edge_top_10x4", (-95, -125, 20, 4), None),
        ("edge_left_4x20", (-115, -115, 4, 30), None),
    ]

    for config_name, lock_rgn, ref_rgn in test_configs:
        locked_features = []
        unlocked_features = []

        for page_num in sorted(pages.keys()):
            items = pages[page_num]
            first_idx = items[0]['idx']
            img_path = os.path.join(BASE_DIR, f"{first_idx:04d}", "full.png")
            if not os.path.exists(img_path):
                continue
            img = np.array(Image.open(img_path))

            for item in items:
                if item['idx'] == first_idx:
                    continue
                row, col = item['row'], item['col']
                cx, cy = grid_center(row, col)

                # Extract lock region
                lx, ly, lw, lh = lock_rgn
                x1, y1 = cx + lx, cy + ly
                x2, y2 = x1 + lw, y1 + lh
                if x1 < 0 or y1 < 0 or x2 >= img.shape[1] or y2 >= img.shape[0]:
                    continue
                lock_patch = img[y1:y2, x1:x2, :3].astype(float)
                lock_brightness = np.mean(lock_patch)

                if ref_rgn:
                    rx, ry, rw, rh = ref_rgn
                    rx1, ry1 = cx + rx, cy + ry
                    rx2, ry2 = rx1 + rw, ry1 + rh
                    if rx1 < 0 or ry1 < 0 or rx2 >= img.shape[1] or ry2 >= img.shape[0]:
                        continue
                    ref_patch = img[ry1:ry2, rx1:rx2, :3].astype(float)
                    ref_brightness = np.mean(ref_patch)
                    feature = lock_brightness - ref_brightness  # negative = lock region darker
                else:
                    feature = lock_brightness

                if item['lock']:
                    locked_features.append(feature)
                else:
                    unlocked_features.append(feature)

        if locked_features and unlocked_features:
            avg_l = np.mean(locked_features)
            avg_u = np.mean(unlocked_features)
            std_l = np.std(locked_features)
            std_u = np.std(unlocked_features)
            combined_std = (std_l + std_u) / 2 + 0.001
            snr = abs(avg_l - avg_u) / combined_std
            # Also compute overlap: what % of unlocked would be misclassified
            # with a simple threshold at (avg_l + avg_u) / 2
            threshold = (avg_l + avg_u) / 2
            if avg_l > avg_u:
                fp = sum(1 for v in unlocked_features if v > threshold)
                fn = sum(1 for v in locked_features if v < threshold)
            else:
                fp = sum(1 for v in unlocked_features if v < threshold)
                fn = sum(1 for v in locked_features if v > threshold)
            accuracy = 1.0 - (fp + fn) / (len(locked_features) + len(unlocked_features))

            print(f"{config_name:30s} SNR={snr:5.2f} acc={accuracy:5.3f} "
                  f"L={avg_l:7.1f}±{std_l:5.1f} U={avg_u:7.1f}±{std_u:5.1f} "
                  f"n={len(locked_features)}+{len(unlocked_features)}")

    # Detailed analysis of the best approach: relative brightness
    print("\n=== Detailed: Lock region vs reference region sweep ===")
    best_results = []

    for ldy in range(-130, -70, 5):
        for ldx in range(-120, -60, 5):
            # Lock region
            lock_rgn = (ldx, ldy, 20, 20)
            # Reference region: same row, shifted right
            ref_rgn = (ldx + 80, ldy, 20, 20)

            locked_features = []
            unlocked_features = []

            for page_num in sorted(pages.keys()):
                items = pages[page_num]
                first_idx = items[0]['idx']
                img_path = os.path.join(BASE_DIR, f"{first_idx:04d}", "full.png")
                if not os.path.exists(img_path):
                    continue
                img = np.array(Image.open(img_path))

                for item in items:
                    if item['idx'] == first_idx:
                        continue
                    row, col = item['row'], item['col']
                    cx, cy = grid_center(row, col)

                    lx, ly, lw, lh = lock_rgn
                    x1, y1 = cx + lx, cy + ly
                    x2, y2 = x1 + lw, y1 + lh

                    rx, ry, rw, rh = ref_rgn
                    rx1, ry1 = cx + rx, cy + ry
                    rx2, ry2 = rx1 + rw, ry1 + rh

                    if (x1 < 0 or y1 < 0 or x2 >= img.shape[1] or y2 >= img.shape[0] or
                        rx1 < 0 or ry1 < 0 or rx2 >= img.shape[1] or ry2 >= img.shape[0]):
                        continue

                    lock_patch = img[y1:y2, x1:x2, :3].astype(float)
                    ref_patch = img[ry1:ry2, rx1:rx2, :3].astype(float)
                    feature = np.mean(lock_patch) - np.mean(ref_patch)

                    if item['lock']:
                        locked_features.append(feature)
                    else:
                        unlocked_features.append(feature)

            if len(locked_features) > 10 and len(unlocked_features) > 10:
                avg_l = np.mean(locked_features)
                avg_u = np.mean(unlocked_features)
                std_l = np.std(locked_features)
                std_u = np.std(unlocked_features)
                combined_std = (std_l + std_u) / 2 + 0.001
                snr = abs(avg_l - avg_u) / combined_std

                threshold = (avg_l + avg_u) / 2
                if avg_l > avg_u:
                    errors = sum(1 for v in unlocked_features if v > threshold) + \
                             sum(1 for v in locked_features if v < threshold)
                else:
                    errors = sum(1 for v in unlocked_features if v < threshold) + \
                             sum(1 for v in locked_features if v > threshold)
                accuracy = 1.0 - errors / (len(locked_features) + len(unlocked_features))

                best_results.append((accuracy, snr, ldx, ldy))

    best_results.sort(reverse=True)
    print(f"{'acc':>6} {'SNR':>6} {'ldx':>5} {'ldy':>5} (4K)  |  {'ldx_1080':>8} {'ldy_1080':>8}")
    for acc, snr, ldx, ldy in best_results[:20]:
        print(f"{acc:6.3f} {snr:6.2f} {ldx:5d} {ldy:5d}      |  {ldx/SCALE:8.1f} {ldy/SCALE:8.1f}")

if __name__ == "__main__":
    main()
