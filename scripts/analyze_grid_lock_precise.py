"""
Precise lock icon analysis on grid cells.

From visual inspection:
- Lock icon is a pink/red padlock in a dark rounded square
- Located in upper-left corner of each grid cell
- Unlocked items have no icon (just the card background)

Grid at 4K (3840x2160), scale=2x from 1920x1080:
  Cell center: (360 + col*290, 506 + row*332)
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
            'astral': a.get('astral_mark', False)
        })

    # For the fine sweep, focus on the lock icon background region
    # From crops: lock icon is roughly at dx=-85...-75, dy=-110...-90 from center (4K)
    # The dark background extends about 25px around the icon
    # Let's do a 1px sweep in the upper-left area

    # Collect pixel data across many pages
    all_locked_pixels = {}  # (dx,dy) -> list of brightness values
    all_unlocked_pixels = {}

    for page_num in sorted(pages.keys()):
        items = pages[page_num]
        # Find a non-selected item to use as image source
        # Use the first item on the page (its full.png shows this page)
        first_idx = items[0]['idx']
        img_path = os.path.join(BASE_DIR, f"{first_idx:04d}", "full.png")
        if not os.path.exists(img_path):
            continue

        img = np.array(Image.open(img_path))

        for item in items:
            # Skip the selected item (has highlight border that affects pixels)
            if item['idx'] == first_idx:
                continue

            row, col = item['row'], item['col']
            cx, cy = grid_center(row, col)

            # Sweep a region where lock icon should be
            for dy in range(-125, -65, 2):
                for dx in range(-105, -50, 2):
                    px = cx + dx
                    py = cy + dy
                    if 0 <= px < img.shape[1] and 0 <= py < img.shape[0]:
                        r, g, b = img[py, px, :3]
                        brightness = (int(r) + int(g) + int(b)) / 3
                        key = (dx, dy)
                        if item['lock']:
                            all_locked_pixels.setdefault(key, []).append(brightness)
                        else:
                            all_unlocked_pixels.setdefault(key, []).append(brightness)

    # Find positions with maximum separation
    results = []
    for key in all_locked_pixels:
        if key not in all_unlocked_pixels:
            continue
        locked_vals = all_locked_pixels[key]
        unlocked_vals = all_unlocked_pixels[key]
        if len(locked_vals) < 10 or len(unlocked_vals) < 10:
            continue

        avg_l = np.mean(locked_vals)
        avg_u = np.mean(unlocked_vals)
        std_l = np.std(locked_vals)
        std_u = np.std(unlocked_vals)

        # Signal-to-noise: difference of means divided by combined std
        combined_std = (std_l + std_u) / 2 + 0.001
        snr = abs(avg_l - avg_u) / combined_std

        results.append((snr, key[0], key[1], avg_l, avg_u, std_l, std_u,
                        len(locked_vals), len(unlocked_vals)))

    results.sort(reverse=True)
    print("=== Top 30 pixel positions by SNR (4K coords, relative to cell center) ===")
    print(f"{'SNR':>6} {'dx':>5} {'dy':>5} | {'avg_L':>7} {'std_L':>7} | {'avg_U':>7} {'std_U':>7} | {'n_L':>4} {'n_U':>4}")
    for snr, dx, dy, avg_l, avg_u, std_l, std_u, nl, nu in results[:30]:
        # Also show 1080p equivalent
        dx_1080 = dx / SCALE
        dy_1080 = dy / SCALE
        print(f"{snr:6.2f} {dx:5d} {dy:5d} | {avg_l:7.1f} {std_l:7.1f} | {avg_u:7.1f} {std_u:7.1f} | {nl:4d} {nu:4d}  (1080p: {dx_1080:.0f}, {dy_1080:.0f})")

    # Also check: what color is the lock icon? (it's pink/red)
    # Look at RGB channels separately for best positions
    print("\n=== RGB analysis for top 5 positions ===")
    for _, dx, dy, *_ in results[:5]:
        locked_r, locked_g, locked_b = [], [], []
        unlocked_r, unlocked_g, unlocked_b = [], [], []

        for page_num in sorted(pages.keys())[:20]:
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
                px_x = cx + dx
                px_y = cy + dy
                if 0 <= px_x < img.shape[1] and 0 <= px_y < img.shape[0]:
                    r, g, b = img[px_y, px_x, :3]
                    if item['lock']:
                        locked_r.append(int(r)); locked_g.append(int(g)); locked_b.append(int(b))
                    else:
                        unlocked_r.append(int(r)); unlocked_g.append(int(g)); unlocked_b.append(int(b))

        if locked_r and unlocked_r:
            print(f"  dx={dx}, dy={dy}:")
            print(f"    Locked   R={np.mean(locked_r):5.1f}±{np.std(locked_r):4.1f}  G={np.mean(locked_g):5.1f}±{np.std(locked_g):4.1f}  B={np.mean(locked_b):5.1f}±{np.std(locked_b):4.1f}")
            print(f"    Unlocked R={np.mean(unlocked_r):5.1f}±{np.std(unlocked_r):4.1f}  G={np.mean(unlocked_g):5.1f}±{np.std(unlocked_g):4.1f}  B={np.mean(unlocked_b):5.1f}±{np.std(unlocked_b):4.1f}")

if __name__ == "__main__":
    main()
