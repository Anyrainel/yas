"""
Analyze grid icons from full.png screenshots to find lock/astral pixel positions.

Grid constants (base 1920x1080):
  GRID_FIRST_X = 180.0, GRID_FIRST_Y = 253.0  (click center)
  GRID_OFFSET_X = 145.0, GRID_OFFSET_Y = 166.0
  8 cols x 5 rows per page

At 4K (3840x2160), multiply by 2.
"""

import json
import os
import sys
from PIL import Image
import numpy as np

BASE_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
SCAN_FILE = "F:/Codes/genshin/yas/target/release/good_export_2026-03-29_01-51-46.json"

# Grid constants scaled for 4K
SCALE = 2.0
GRID_FIRST_X = int(180.0 * SCALE)
GRID_FIRST_Y = int(253.0 * SCALE)
GRID_OFFSET_X = int(145.0 * SCALE)
GRID_OFFSET_Y = int(166.0 * SCALE)
GRID_COLS = 8
GRID_ROWS = 5

def grid_center(row, col):
    """Get pixel center of grid cell at (row, col) in 4K coords."""
    x = GRID_FIRST_X + col * GRID_OFFSET_X
    y = GRID_FIRST_Y + row * GRID_OFFSET_Y
    return x, y

def extract_cell_region(img, row, col, dx_range, dy_range):
    """Extract pixel region relative to grid cell center."""
    cx, cy = grid_center(row, col)
    x1 = cx + dx_range[0]
    y1 = cy + dy_range[0]
    x2 = cx + dx_range[1]
    y2 = cy + dy_range[1]
    return img.crop((x1, y1, x2, y2))

def analyze_lock_region():
    """Extract the upper-left region of each grid cell to find lock icons."""
    with open(SCAN_FILE) as f:
        data = json.load(f)
    arts = data['artifacts']

    # Use item 0 (page 0, locked) to extract the full grid
    img = Image.open(os.path.join(BASE_DIR, "0000", "full.png"))
    arr = np.array(img)

    # Save individual cell crops for visual inspection
    out_dir = "F:/Codes/genshin/yas/scripts/grid_crops"
    os.makedirs(out_dir, exist_ok=True)

    # Crop the upper-left corner of each cell on page 0
    # The lock icon should be in the upper-left of the artifact card
    # Let's crop a generous region: -70 to +70 from center X, -80 to +10 from center Y (upper part)
    for row in range(GRID_ROWS):
        for col in range(GRID_COLS):
            idx = row * GRID_COLS + col
            is_locked = arts[idx].get('lock', False)

            # Crop upper-left quadrant of cell (where lock icon would be)
            cx, cy = grid_center(row, col)
            # The artifact card is roughly 130x160 px at 4K
            # Lock icon is in upper-left corner
            x1 = cx - 120
            y1 = cy - 140
            x2 = cx + 120
            y2 = cy + 140
            cell = img.crop((x1, y1, x2, y2))
            lock_str = "LOCKED" if is_locked else "UNLOCKED"
            cell.save(os.path.join(out_dir, f"page0_r{row}c{col}_{lock_str}.png"))

    print(f"Saved cell crops to {out_dir}")

    # Also save the full grid region
    grid_region = img.crop((0, 0, 1300 * 2, 1080 * 2))
    grid_region.save(os.path.join(out_dir, "grid_full_page0.png"))

def scan_lock_pixels():
    """Systematically scan potential lock icon pixels across cells with known states."""
    with open(SCAN_FILE) as f:
        data = json.load(f)
    arts = data['artifacts']

    # Collect (image_path, grid_positions_with_lock_state) for pages with mixed lock states
    # Item idx -> page, row, col
    # We want pages where both locked and unlocked items exist

    pages_with_mix = {}
    for i, a in enumerate(arts):
        page = i // 40
        pos = i % 40
        row, col = pos // 8, pos % 8
        if page not in pages_with_mix:
            pages_with_mix[page] = {'locked': [], 'unlocked': []}
        key = 'locked' if a.get('lock') else 'unlocked'
        pages_with_mix[page][key].append((row, col, i))

    # Filter to pages with both states
    mixed_pages = {p: v for p, v in pages_with_mix.items()
                   if v['locked'] and v['unlocked']}

    print(f"Found {len(mixed_pages)} pages with mixed lock states")

    # For each mixed page, load the full.png of any item on that page
    # and analyze pixel differences between locked and unlocked cells

    # We need the full.png that shows this page. The full.png for item i
    # shows the page containing item i. So any item on page P gives us that page's screenshot.

    results = []

    for page_num in sorted(mixed_pages.keys())[:5]:  # first 5 mixed pages
        info = mixed_pages[page_num]
        # Use the first item's screenshot as representative
        first_idx = info['locked'][0][2] if info['locked'] else info['unlocked'][0][2]
        img_path = os.path.join(BASE_DIR, f"{first_idx:04d}", "full.png")
        if not os.path.exists(img_path):
            continue

        img = Image.open(img_path)
        arr = np.array(img)

        print(f"\n=== Page {page_num} (using item {first_idx}) ===")
        print(f"  Locked: {[(r,c) for r,c,_ in info['locked'][:5]]}")
        print(f"  Unlocked: {[(r,c) for r,c,_ in info['unlocked'][:5]]}")

        # Check pixels in the upper-left corner of each cell
        # Try a sweep of positions relative to cell center
        # Lock icon is "almost top-left corner" of the card

        # Sample a grid of relative positions
        for dy in range(-130, -50, 10):
            for dx in range(-110, -30, 10):
                locked_vals = []
                unlocked_vals = []

                for row, col, idx in info['locked']:
                    # Skip selected item (it has a highlight)
                    if idx == first_idx:
                        continue
                    cx, cy = grid_center(row, col)
                    px = cx + dx
                    py = cy + dy
                    if 0 <= px < arr.shape[1] and 0 <= py < arr.shape[0]:
                        r, g, b = arr[py, px, :3]
                        brightness = (int(r) + int(g) + int(b)) / 3
                        locked_vals.append(brightness)

                for row, col, idx in info['unlocked']:
                    if idx == first_idx:
                        continue
                    cx, cy = grid_center(row, col)
                    px = cx + dx
                    py = cy + dy
                    if 0 <= px < arr.shape[1] and 0 <= py < arr.shape[0]:
                        r, g, b = arr[py, px, :3]
                        brightness = (int(r) + int(g) + int(b)) / 3
                        unlocked_vals.append(brightness)

                if locked_vals and unlocked_vals:
                    avg_locked = sum(locked_vals) / len(locked_vals)
                    avg_unlocked = sum(unlocked_vals) / len(unlocked_vals)
                    diff = abs(avg_locked - avg_unlocked)
                    if diff > 30:  # significant difference
                        results.append((diff, dx, dy, avg_locked, avg_unlocked, page_num))

    # Sort by difference
    results.sort(reverse=True)
    print("\n=== Top discriminating pixel positions (dx, dy from cell center) ===")
    for diff, dx, dy, avg_l, avg_u, page in results[:30]:
        print(f"  dx={dx:4d} dy={dy:4d} | locked_avg={avg_l:6.1f} unlocked_avg={avg_u:6.1f} | diff={diff:5.1f} | page={page}")

if __name__ == "__main__":
    if "--crops" in sys.argv:
        analyze_lock_region()
    else:
        scan_lock_pixels()
