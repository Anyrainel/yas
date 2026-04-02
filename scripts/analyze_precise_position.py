"""
Measure the dark square signal when we KNOW the exact icon position.

For locked items, the pink centroid tells us exactly where the lock icon is.
Using this precise position:
1. How dark is the square bg vs the surrounding card?
2. How consistent is this across rarities?
3. If we could grid-align, how much room would we have?

Also: measure the actual scroll offset per page to understand grid precision.
"""

import json
import os
import numpy as np
from PIL import Image
from multiprocessing import Pool, cpu_count
from collections import defaultdict

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

def is_pink(r, g, b):
    ri, gi, bi = int(r), int(g), int(b)
    return ri > 180 and (ri - gi) > 60 and ri > bi + 50 and bi > 70

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
        rarity = _rarity.get(idx, 5)

        # For locked items: find pink centroid, then measure surroundings
        if is_locked:
            x1 = max(0, cx - 130)
            y1 = max(0, cy - 165)
            x2 = min(w, cx - 55)
            y2 = min(h, cy - 55)
            patch = img[y1:y2, x1:x2, :3]
            ri = patch[:,:,0].astype(np.int16)
            gi = patch[:,:,1].astype(np.int16)
            bi = patch[:,:,2].astype(np.int16)
            mask = (ri > 180) & ((ri-gi) > 60) & ((ri-bi) > 50) & (bi > 70)
            if np.sum(mask) < 10:
                continue

            ys, xs = np.where(mask)
            pcx = float(np.mean(xs)) + x1  # image coords
            pcy = float(np.mean(ys)) + y1

            # Offset from expected position
            expected_x = cx - 89
            expected_y = cy - 112
            offset_x = pcx - expected_x
            offset_y = pcy - expected_y

            # Sample brightness at precise positions relative to centroid:
            # Inside dark square (at centroid ± small offset)
            # Outside dark square (above, right of dark square)
            samples = {}
            for label, dx, dy in [
                ("inside_center", 0, 0),
                ("inside_left", -15, 0),
                ("inside_right", +15, 0),
                ("inside_top", 0, -15),
                ("inside_bot", 0, +15),
                # Just outside dark square edges (should be rarity bg color)
                ("outside_above", 0, -30),
                ("outside_left", -30, 0),
                ("outside_right", +30, 0),
                ("outside_below", 0, +30),
                # Card background (well outside any icon)
                ("card_bg", +50, 0),
            ]:
                sx = int(pcx + dx)
                sy = int(pcy + dy)
                if 0 <= sx < w and 0 <= sy < h:
                    r, g, b = img[sy, sx, :3]
                    samples[label] = (int(r), int(g), int(b),
                                     (int(r) + int(g) + int(b)) / 3)

            results.append({
                'idx': idx, 'locked': True, 'rarity': rarity,
                'offset_x': round(offset_x, 1),
                'offset_y': round(offset_y, 1),
                'samples': samples,
                'page': page, 'row': row, 'col': col,
                'scan_idx': scan_idx,
            })

        else:
            # For unlocked items: sample at expected lock position
            # (no dark square should be there)
            expected_x = cx - 89
            expected_y = cy - 112
            samples = {}
            for label, dx, dy in [
                ("at_lock_pos", 0, 0),
                ("above_lock", 0, -30),
                ("card_bg", +50, 0),
            ]:
                sx = int(expected_x + dx)
                sy = int(expected_y + dy)
                if 0 <= sx < w and 0 <= sy < h:
                    r, g, b = img[sy, sx, :3]
                    samples[label] = (int(r), int(g), int(b),
                                     (int(r) + int(g) + int(b)) / 3)

            results.append({
                'idx': idx, 'locked': False, 'rarity': rarity,
                'samples': samples,
                'page': page, 'row': row, 'col': col,
            })

    return results


def main():
    total_arts = len(_gt)
    print(f"Analyzing {total_arts} images...")

    all_results = []
    with Pool(min(cpu_count(), 8)) as pool:
        for batch in pool.imap_unordered(process_image, range(total_arts), chunksize=20):
            all_results.extend(batch)

    locked = [r for r in all_results if r['locked']]
    unlocked = [r for r in all_results if not r['locked']]

    print(f"Locked samples: {len(locked):,}")
    print(f"Unlocked samples: {len(unlocked):,}")

    # === POSITIONING ACCURACY ===
    print(f"\n{'='*65}")
    print(f"  CENTROID OFFSET FROM EXPECTED POSITION (at 4K)")
    print(f"  Expected lock center: cell_center + (-89, -112)")
    print(f"{'='*65}")

    for row in range(5):
        row_data = [r for r in locked if r['row'] == row]
        if not row_data:
            continue
        ox = [r['offset_x'] for r in row_data]
        oy = [r['offset_y'] for r in row_data]
        print(f"\n  Row {row} (n={len(row_data):,}):")
        print(f"    X offset: mean={np.mean(ox):+5.1f}  std={np.std(ox):4.1f}  "
              f"range=[{min(ox):+.0f}, {max(ox):+.0f}]")
        print(f"    Y offset: mean={np.mean(oy):+5.1f}  std={np.std(oy):4.1f}  "
              f"range=[{min(oy):+.0f}, {max(oy):+.0f}]")

    # Per-page consistency
    page_offsets = defaultdict(list)
    for r in locked:
        page_offsets[(r['page'], r['row'])].append(r['offset_y'])

    within_page_stds = []
    for key, offsets in page_offsets.items():
        if len(offsets) >= 3:
            within_page_stds.append(np.std(offsets))

    if within_page_stds:
        print(f"\n  Within-page Y consistency (same page+row):")
        print(f"    mean_std={np.mean(within_page_stds):.2f}px  max_std={max(within_page_stds):.2f}px")
        print(f"    (This is how precise the grid is within a single row on one page)")

    # === DARK SQUARE BRIGHTNESS AT PRECISE POSITION ===
    print(f"\n{'='*65}")
    print(f"  DARK SQUARE BRIGHTNESS (at precisely-known centroid)")
    print(f"{'='*65}")

    for rarity in [5, 4]:
        r_locked = [r for r in locked if r['rarity'] == rarity and 'inside_center' in r['samples']]
        r_unlocked = [r for r in unlocked if r['rarity'] == rarity and 'at_lock_pos' in r['samples']]

        if not r_locked:
            continue

        print(f"\n  --- Rarity {rarity} ---")

        # Inside dark square (locked)
        inside = [r['samples']['inside_center'][3] for r in r_locked]
        # Outside dark square but same card (locked)
        outside_above = [r['samples']['outside_above'][3] for r in r_locked if 'outside_above' in r['samples']]
        outside_right = [r['samples']['outside_right'][3] for r in r_locked if 'outside_right' in r['samples']]
        # Card background at lock position (unlocked = no dark square)
        bg_at_pos = [r['samples']['at_lock_pos'][3] for r in r_unlocked if 'at_lock_pos' in r['samples']]

        print(f"\n  LOCKED: Inside dark square (brightness):")
        print(f"    min={min(inside):.0f}  p5={np.percentile(inside,5):.0f}  "
              f"median={np.median(inside):.0f}  p95={np.percentile(inside,95):.0f}  max={max(inside):.0f}")

        if outside_above:
            print(f"  LOCKED: Just above dark square (card bg):")
            print(f"    min={min(outside_above):.0f}  p5={np.percentile(outside_above,5):.0f}  "
                  f"median={np.median(outside_above):.0f}  p95={np.percentile(outside_above,95):.0f}  max={max(outside_above):.0f}")

            # Contrast
            contrasts = [o - i for o, i in zip(outside_above, inside)]
            print(f"  Contrast (above - inside):")
            print(f"    min={min(contrasts):.0f}  p5={np.percentile(contrasts,5):.0f}  "
                  f"median={np.median(contrasts):.0f}  max={max(contrasts):.0f}")

        if bg_at_pos:
            print(f"\n  UNLOCKED: Card bg at same position (no dark square):")
            print(f"    min={min(bg_at_pos):.0f}  p5={np.percentile(bg_at_pos,5):.0f}  "
                  f"median={np.median(bg_at_pos):.0f}  p95={np.percentile(bg_at_pos,95):.0f}  max={max(bg_at_pos):.0f}")

        # === THE GAP ===
        if inside and bg_at_pos:
            max_inside = max(inside)
            min_bg = min(bg_at_pos)
            print(f"\n  ** GAP: brightest dark-square pixel = {max_inside:.0f}")
            print(f"  ** GAP: darkest unlocked-bg pixel   = {min_bg:.0f}")
            print(f"  ** GAP size: {min_bg - max_inside:.0f} brightness units")
            if min_bg > max_inside:
                print(f"  ** Clean separation! Threshold anywhere in [{max_inside:.0f}, {min_bg:.0f}] works.")
            else:
                print(f"  ** OVERLAP — brightness alone can't separate.")

    # === RGB at precise position ===
    print(f"\n{'='*65}")
    print(f"  RGB VALUES AT DARK SQUARE CENTER (precise position)")
    print(f"  This is what a color check would see with perfect alignment")
    print(f"{'='*65}")

    for rarity in [5, 4]:
        r_locked = [r for r in locked if r['rarity'] == rarity and 'inside_center' in r['samples']]
        if not r_locked:
            continue
        print(f"\n  Rarity {rarity} — inside dark square center (R, G, B):")
        rs = [r['samples']['inside_center'][0] for r in r_locked]
        gs = [r['samples']['inside_center'][1] for r in r_locked]
        bs = [r['samples']['inside_center'][2] for r in r_locked]
        print(f"    R: min={min(rs):3d}  p5={np.percentile(rs,5):5.0f}  median={np.median(rs):5.0f}  max={max(rs):3d}")
        print(f"    G: min={min(gs):3d}  p5={np.percentile(gs,5):5.0f}  median={np.median(gs):5.0f}  max={max(gs):3d}")
        print(f"    B: min={min(bs):3d}  p5={np.percentile(bs,5):5.0f}  median={np.median(bs):5.0f}  max={max(bs):3d}")

        # What does unlocked look like at same position (but position is from expected, not centroid)
        r_unlocked = [r for r in unlocked if r['rarity'] == rarity and 'at_lock_pos' in r['samples']]
        if r_unlocked:
            print(f"  Rarity {rarity} — unlocked card at expected lock position (R, G, B):")
            rs2 = [r['samples']['at_lock_pos'][0] for r in r_unlocked]
            gs2 = [r['samples']['at_lock_pos'][1] for r in r_unlocked]
            bs2 = [r['samples']['at_lock_pos'][2] for r in r_unlocked]
            print(f"    R: min={min(rs2):3d}  p5={np.percentile(rs2,5):5.0f}  median={np.median(rs2):5.0f}  max={max(rs2):3d}")
            print(f"    G: min={min(gs2):3d}  p5={np.percentile(gs2,5):5.0f}  median={np.median(gs2):5.0f}  max={max(gs2):3d}")
            print(f"    B: min={min(bs2):3d}  p5={np.percentile(bs2,5):5.0f}  median={np.median(bs2):5.0f}  max={max(bs2):3d}")


if __name__ == "__main__":
    main()
