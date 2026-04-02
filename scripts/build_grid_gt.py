"""
Build corrected ground truth for lock/astral by analyzing grid icons.

For each artifact index, it appears on 39 different full.png screenshots
(every other item on the same page). We run grid detection on each and
take a majority vote to determine the true lock/astral state.

Output: grid_ground_truth.json with per-item lock/astral states.
"""

import json
import os
import sys
import numpy as np
from PIL import Image
from collections import defaultdict

BASE_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
SCAN_FILE = "F:/Codes/genshin/yas/target/release/good_export_2026-03-29_01-51-46.json"
OUTPUT_FILE = "F:/Codes/genshin/yas/scripts/grid_ground_truth.json"

SCALE = 2.0
GX = int(180.0 * SCALE)
GY = int(253.0 * SCALE)
OX = int(145.0 * SCALE)
OY = int(166.0 * SCALE)

def gc(r, c):
    return GX + c * OX, GY + r * OY

def is_pink(r, g, b):
    ri, gi, bi = int(r), int(g), int(b)
    return ri > 180 and (ri - gi) > 60 and ri > bi + 50 and bi > 70

def is_yellow(r, g, b):
    return int(r) > 220 and int(g) > 170 and int(b) < 80

def count_cent(img, x1, y1, x2, y2, fn):
    x1, y1 = max(0, x1), max(0, y1)
    x2, y2 = min(img.shape[1], x2), min(img.shape[0], y2)
    sx, sy, n = 0, 0, 0
    for py in range(y1, y2):
        for px in range(x1, x2):
            if fn(*img[py, px, :3]):
                sx += px; sy += py; n += 1
    return n, (sx / n, sy / n) if n else None

def count_px(img, x1, y1, x2, y2, fn):
    x1, y1 = max(0, x1), max(0, y1)
    x2, y2 = min(img.shape[1], x2), min(img.shape[0], y2)
    return sum(1 for py in range(y1, y2) for px in range(x1, x2) if fn(*img[py, px, :3]))


def main():
    with open(SCAN_FILE) as f:
        arts = json.load(f)['artifacts']

    total_arts = len(arts)
    print(f"Total artifacts: {total_arts}")

    # Build page -> items mapping
    page_items = {}
    for i, a in enumerate(arts):
        page = i // 40
        pos = i % 40
        row, col = pos // 8, pos % 8
        page_items.setdefault(page, []).append({
            'idx': i, 'row': row, 'col': col,
        })

    # Per-item vote accumulators
    # For each item: list of (pink_count, yellow_count) from each screenshot
    item_votes = defaultdict(list)  # idx -> [(pink, yellow), ...]

    for scan_idx in range(total_arts):
        img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
        if not os.path.exists(img_path):
            continue

        page = scan_idx // 40
        items = page_items.get(page, [])
        if not items:
            continue

        img_arr = np.array(Image.open(img_path))

        for item in items:
            if item['idx'] == scan_idx:
                continue  # skip selected item

            cx, cy = gc(item['row'], item['col'])
            pk, pc = count_cent(img_arr, cx - 130, cy - 165, cx - 55, cy - 55, is_pink)

            yw = 0
            if pk >= 10 and pc:
                lx, ly = pc
                yw = count_px(img_arr, int(lx) - 30, int(ly) + 15,
                              int(lx) + 30, int(ly) + 80, is_yellow)

            item_votes[item['idx']].append((pk, yw))

        if scan_idx % 100 == 0:
            print(f"  Processing {scan_idx}/{total_arts}...", file=sys.stderr)

    # Build ground truth from votes
    gt = []
    disagreements = []

    for i, a in enumerate(arts):
        votes = item_votes.get(i, [])
        if not votes:
            # No votes (item never appeared as non-selected) — use scan data
            gt.append({
                'idx': i,
                'lock': a.get('lock', False),
                'astralMark': a.get('astralMark', False),
                'source': 'scan',
                'votes': 0,
            })
            continue

        lock_votes = sum(1 for pk, yw in votes if pk >= 10)
        astral_votes = sum(1 for pk, yw in votes if pk >= 10 and yw >= 200)
        total_votes = len(votes)

        # Majority vote
        grid_lock = lock_votes > total_votes / 2
        grid_astral = astral_votes > total_votes / 2

        scan_lock = a.get('lock', False)
        scan_astral = a.get('astralMark', False)

        # Check for disagreements
        if grid_lock != scan_lock or grid_astral != scan_astral:
            pink_vals = [pk for pk, yw in votes]
            yellow_vals = [yw for pk, yw in votes]
            disagreements.append({
                'idx': i,
                'scan_lock': scan_lock, 'grid_lock': grid_lock,
                'scan_astral': scan_astral, 'grid_astral': grid_astral,
                'lock_votes': f"{lock_votes}/{total_votes}",
                'astral_votes': f"{astral_votes}/{total_votes}",
                'pink_range': f"{min(pink_vals)}-{max(pink_vals)}",
                'yellow_range': f"{min(yellow_vals)}-{max(yellow_vals)}",
            })

        gt.append({
            'idx': i,
            'lock': grid_lock,
            'astralMark': grid_astral,
            'source': 'grid_majority',
            'votes': total_votes,
            'lock_votes': lock_votes,
            'astral_votes': astral_votes,
        })

    # Save ground truth
    output = {
        'description': 'Ground truth derived from grid icon pixel analysis (majority vote across all full.png screenshots)',
        'algorithm': {
            'lock': 'Count pink pixels (R>180, R-G>60, R-B>50, B>70) in search window; >=10 = locked',
            'astral': 'Count bright yellow pixels (R>220, G>170, B<80) below lock centroid; >=200 = astral',
            'search_window_lock': '75x55px at 1080p, positioned to cover scroll drift',
        },
        'total_items': total_arts,
        'items': gt,
    }

    with open(OUTPUT_FILE, 'w') as f:
        json.dump(output, f, indent=2)

    print(f"\nSaved ground truth to {OUTPUT_FILE}")
    print(f"Total items: {total_arts}")
    print(f"Items with grid votes: {sum(1 for g in gt if g['source'] == 'grid_majority')}")

    # Report disagreements
    if disagreements:
        print(f"\n=== Disagreements between scan and grid ({len(disagreements)}) ===")
        for d in disagreements:
            changes = []
            if d['scan_lock'] != d['grid_lock']:
                changes.append(f"lock: scan={d['scan_lock']} -> grid={d['grid_lock']} ({d['lock_votes']} votes, pink={d['pink_range']})")
            if d['scan_astral'] != d['grid_astral']:
                changes.append(f"astral: scan={d['scan_astral']} -> grid={d['grid_astral']} ({d['astral_votes']} votes, yellow={d['yellow_range']})")
            print(f"  idx={d['idx']:4d}: {'; '.join(changes)}")
    else:
        print("\nNo disagreements between scan and grid!")


if __name__ == "__main__":
    main()
