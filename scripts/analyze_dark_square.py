"""
Analyze the dark square overlay as primary detection signal.

The lock and astral icons both sit on a dark semi-transparent square.
This square contrasts strongly with the 3 possible backgrounds:
  - 5-star gold (~160-180 brightness)
  - 4-star purple (~100-130 brightness)
  - Elixir dark purple (similar but shifted)

Questions to answer:
1. How precisely can we locate the dark square edges?
2. What's the brightness contrast vs each rarity background?
3. Can we use dark square detection alone, then check icon color loosely?
4. How consistent is grid positioning across scroll offsets?
"""

import json
import os
import numpy as np
from PIL import Image
from multiprocessing import Pool, cpu_count

BASE_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
GT_FILE = "F:/Codes/genshin/yas/scripts/grid_ground_truth.json"
SCAN_FILE = "F:/Codes/genshin/yas/target/release/good_export_2026-03-29_01-51-46.json"

SCALE = 2.0
GX = int(180.0 * SCALE)
GY = int(253.0 * SCALE)
OX = int(145.0 * SCALE)
OY = int(166.0 * SCALE)

with open(GT_FILE) as f:
    _gt = json.load(f)['items']
_gt_lock = {g['idx']: g['lock'] for g in _gt}
_gt_astral = {g['idx']: g['astralMark'] for g in _gt}

with open(SCAN_FILE) as f:
    _arts = json.load(f)['artifacts']
_rarity = {i: a['rarity'] for i, a in enumerate(_arts)}

_page_items = {}
for g in _gt:
    i = g['idx']
    page = i // 40
    pos = i % 40
    row, col = pos // 8, pos % 8
    _page_items.setdefault(page, []).append((i, row, col))

def gc(r, c):
    return GX + c * OX, GY + r * OY


def process_image(scan_idx):
    img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
    if not os.path.exists(img_path):
        return []

    page = scan_idx // 40
    items = _page_items.get(page, [])
    img = np.array(Image.open(img_path))
    h, w = img.shape[:2]

    results = []

    for idx, row, col in items:
        if idx == scan_idx:
            continue

        cx, cy = gc(row, col)
        is_locked = _gt_lock[idx]
        is_astral = _gt_astral[idx]
        rarity = _rarity.get(idx, 5)

        # Extract a column of brightness values along the left edge of the card,
        # centered on the expected dark square position.
        # This gives us a 1D brightness profile that should show:
        #   bright background -> dark square -> bright background (-> dark square if astral -> bright)

        # Use x = cx - 89 (center of icon column at 4K), sweep vertically
        icon_x = cx - 89

        # Sample a vertical stripe (3px wide for noise reduction) from cy-170 to cy-10
        x1 = max(0, icon_x - 1)
        x2 = min(w, icon_x + 2)
        y1 = max(0, cy - 170)
        y2 = min(h, cy - 10)

        stripe = img[y1:y2, x1:x2, :3].astype(np.float32)
        brightness = np.mean(stripe, axis=(1, 2))  # average across 3px width and RGB

        # Also sample a horizontal stripe through the icon center
        icon_y_lock = cy - 112  # expected lock center Y at 4K
        hy1 = max(0, icon_y_lock - 1)
        hy2 = min(h, icon_y_lock + 2)
        hx1 = max(0, cx - 130)
        hx2 = min(w, cx - 50)

        hstripe = img[hy1:hy2, hx1:hx2, :3].astype(np.float32)
        h_brightness = np.mean(hstripe, axis=(0, 2))

        # Find dark square edges in vertical profile
        # The dark square should create a clear dip in brightness
        # Look for transitions: bright->dark and dark->bright

        # Compute gradient
        grad = np.diff(brightness)

        # Find strongest negative edge (bright->dark = top of dark square)
        # and strongest positive edge (dark->bright = bottom of dark square)
        # in the expected range

        # Expected dark square top at 4K: cy - 130 to cy - 120 (relative to y1)
        # Expected dark square bottom: cy - 85 to cy - 75

        # Convert to stripe indices
        expected_top_range = (0, len(brightness) // 2)
        expected_bot_range = (len(brightness) // 4, len(brightness))

        if len(grad) > 10:
            # Top edge: strongest negative gradient in upper half
            top_region = grad[:len(grad)//2]
            top_edge_idx = int(np.argmin(top_region))
            top_edge_strength = float(-top_region[top_edge_idx])

            # Bottom edge of lock dark square: strongest positive gradient in middle
            mid_start = max(0, top_edge_idx + 10)
            mid_region = grad[mid_start:mid_start + 80]
            if len(mid_region) > 0:
                bot_edge_rel = int(np.argmax(mid_region))
                bot_edge_idx = mid_start + bot_edge_rel
                bot_edge_strength = float(mid_region[bot_edge_rel])
            else:
                bot_edge_idx = 0
                bot_edge_strength = 0

            dark_square_height = bot_edge_idx - top_edge_idx

            # Brightness inside vs outside dark square
            if top_edge_idx + 5 < bot_edge_idx - 5:
                inside_brightness = float(np.mean(brightness[top_edge_idx+5:bot_edge_idx-5]))
            else:
                inside_brightness = float(np.mean(brightness))

            # Background brightness (above and below dark square)
            above = brightness[:max(1, top_edge_idx-3)]
            bg_brightness = float(np.mean(above)) if len(above) > 0 else 0

            contrast = bg_brightness - inside_brightness

            results.append({
                'idx': idx, 'locked': is_locked, 'astral': is_astral,
                'rarity': rarity,
                'top_edge_y': int(y1 + top_edge_idx),
                'bot_edge_y': int(y1 + bot_edge_idx),
                'top_strength': round(top_edge_strength, 1),
                'bot_strength': round(bot_edge_strength, 1),
                'dark_sq_height': dark_square_height,
                'inside_bright': round(inside_brightness, 1),
                'bg_bright': round(bg_brightness, 1),
                'contrast': round(contrast, 1),
                'scan_idx': scan_idx,
                'row': row, 'col': col,
                # Expected position
                'expected_cy': cy,
            })

    return results


def main():
    total_arts = len(_gt)
    print(f"Analyzing dark square profiles for {total_arts} images...")

    all_results = []
    with Pool(min(cpu_count(), 8)) as pool:
        for batch in pool.imap_unordered(process_image, range(total_arts), chunksize=20):
            all_results.extend(batch)

    print(f"Total samples: {len(all_results):,}")

    # Separate by state and rarity
    for rarity in [5, 4]:
        locked = [r for r in all_results if r['locked'] and r['rarity'] == rarity and not r['astral']]
        unlocked = [r for r in all_results if not r['locked'] and r['rarity'] == rarity]
        astral = [r for r in all_results if r['astral'] and r['rarity'] == rarity]

        print(f"\n{'='*65}")
        print(f"  RARITY {rarity} STAR")
        print(f"{'='*65}")

        if unlocked:
            bg = [r['bg_bright'] for r in unlocked]
            contrast = [r['contrast'] for r in unlocked]
            print(f"\n  UNLOCKED (n={len(unlocked):,}):")
            print(f"    Background brightness: min={min(bg):.0f} p5={np.percentile(bg,5):.0f} "
                  f"median={np.median(bg):.0f} p95={np.percentile(bg,95):.0f} max={max(bg):.0f}")
            print(f"    Contrast (bg-inside):  min={min(contrast):.0f} p5={np.percentile(contrast,5):.0f} "
                  f"median={np.median(contrast):.0f} max={max(contrast):.0f}")

        if locked:
            bg = [r['bg_bright'] for r in locked]
            inside = [r['inside_bright'] for r in locked]
            contrast = [r['contrast'] for r in locked]
            heights = [r['dark_sq_height'] for r in locked]
            top_str = [r['top_strength'] for r in locked]

            print(f"\n  LOCKED (n={len(locked):,}):")
            print(f"    Background brightness: min={min(bg):.0f} p5={np.percentile(bg,5):.0f} "
                  f"median={np.median(bg):.0f} max={max(bg):.0f}")
            print(f"    Inside dark sq bright: min={min(inside):.0f} p5={np.percentile(inside,5):.0f} "
                  f"median={np.median(inside):.0f} max={max(inside):.0f}")
            print(f"    Contrast:              min={min(contrast):.0f} p5={np.percentile(contrast,5):.0f} "
                  f"median={np.median(contrast):.0f} max={max(contrast):.0f}")
            print(f"    Dark sq height (px):   min={min(heights)} p5={np.percentile(heights,5):.0f} "
                  f"median={np.median(heights):.0f} max={max(heights)}")
            print(f"    Top edge strength:     min={min(top_str):.0f} p5={np.percentile(top_str,5):.0f} "
                  f"median={np.median(top_str):.0f} max={max(top_str):.0f}")

        if astral:
            print(f"\n  ASTRAL (n={len(astral):,}):")
            contrast = [r['contrast'] for r in astral]
            print(f"    Contrast:              min={min(contrast):.0f} median={np.median(contrast):.0f}")

    # === GRID POSITIONING ACCURACY ===
    # For locked items, the top edge of the dark square gives us the actual icon position.
    # Compare this to the expected position across different rows and pages.
    print(f"\n{'='*65}")
    print(f"  GRID POSITIONING ACCURACY")
    print(f"  (How far is the dark square from expected position?)")
    print(f"{'='*65}")

    locked_results = [r for r in all_results if r['locked'] and r['contrast'] > 20]

    # Group by (page, row) — same page+row should have same offset
    from collections import defaultdict
    page_row_offsets = defaultdict(list)
    for r in locked_results:
        page = r['idx'] // 40
        expected_top = r['expected_cy'] - 130  # expected dark sq top at 4K
        actual_top = r['top_edge_y']
        offset = actual_top - expected_top
        page_row_offsets[(page, r['row'])].append(offset)

    # Show offset distribution
    all_offsets = []
    for (page, row), offsets in sorted(page_row_offsets.items()):
        mean_off = np.mean(offsets)
        all_offsets.append((page, row, mean_off, np.std(offsets), len(offsets)))

    print(f"\n  Offset from expected position (positive = shifted down):")
    print(f"  Total (page,row) groups: {len(all_offsets)}")

    # Group by row to see systematic drift
    for row in range(5):
        row_offsets = [o[2] for o in all_offsets if o[1] == row]
        if row_offsets:
            print(f"    Row {row}: mean_offset={np.mean(row_offsets):+5.1f}px  "
                  f"std={np.std(row_offsets):4.1f}  range=[{min(row_offsets):+.0f}, {max(row_offsets):+.0f}]  "
                  f"(n={len(row_offsets)} page-groups)")

    # Within a single page, how consistent are offsets across columns?
    print(f"\n  Within-page consistency (std of offset across columns on same page+row):")
    stds = [o[3] for o in all_offsets if o[4] >= 5]  # need enough samples
    if stds:
        print(f"    mean_std={np.mean(stds):.2f}px  max_std={max(stds):.2f}px")
        print(f"    (lower = more consistent positioning within a row)")


if __name__ == "__main__":
    main()
