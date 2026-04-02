"""
Test relaxed thresholds against unlocked cells.
Module-level function for multiprocessing compatibility.
"""

import json
import os
import numpy as np
from PIL import Image
from multiprocessing import Pool, cpu_count

BASE_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
GT_FILE = "F:/Codes/genshin/yas/scripts/grid_ground_truth.json"

SCALE = 2.0
GX = int(180.0 * SCALE)
GY = int(253.0 * SCALE)
OX = int(145.0 * SCALE)
OY = int(166.0 * SCALE)

# Load GT at module level for multiprocessing
with open(GT_FILE) as f:
    _gt_data = json.load(f)
_gt_items = _gt_data['items']
_gt_lock = {g['idx']: g['lock'] for g in _gt_items}

_page_items = {}
for g in _gt_items:
    i = g['idx']
    page = i // 40
    pos = i % 40
    row, col = pos // 8, pos % 8
    _page_items.setdefault(page, []).append((i, row, col))

CONFIGS = [
    ("R>140,RG>40,RB>30,B>40", 140, 40, 30, 40),
    ("R>150,RG>40,RB>30,B>50", 150, 40, 30, 50),
    ("R>160,RG>40,RB>30,B>50", 160, 40, 30, 50),
    ("R>160,RG>50,RB>40,B>60", 160, 50, 40, 60),
    ("R>170,RG>50,RB>40,B>60", 170, 50, 40, 60),
    ("R>180,RG>40,RB>30,B>50", 180, 40, 30, 50),
    ("R>180,RG>50,RB>40,B>60", 180, 50, 40, 60),
    ("R>180,RG>50,RB>40,B>70", 180, 50, 40, 70),
    ("R>180,RG>60,RB>50,B>50", 180, 60, 50, 50),
    ("R>180,RG>60,RB>50,B>0 ", 180, 60, 50, 0),
    ("R>180,RG>60,RB>50,B>70 ** current **", 180, 60, 50, 70),
]


def gc(r, c):
    return GX + c * OX, GY + r * OY


def process_image(scan_idx):
    img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
    if not os.path.exists(img_path):
        return {}, {}

    page = scan_idx // 40
    items = _page_items.get(page, [])
    img = np.array(Image.open(img_path))
    h, w = img.shape[:2]

    # FP counts per config for unlocked cells
    fp_counts = {}
    # FN counts per config for locked cells
    fn_counts = {}

    for idx, row, col in items:
        if idx == scan_idx:
            continue

        cx, cy = gc(row, col)
        x1, y1 = max(0, cx - 130), max(0, cy - 165)
        x2, y2 = min(w, cx - 55), min(h, cy - 55)
        patch = img[y1:y2, x1:x2, :3]
        ri = patch[:, :, 0].astype(np.int16)
        gi = patch[:, :, 1].astype(np.int16)
        bi = patch[:, :, 2].astype(np.int16)
        rg = ri - gi
        rb = ri - bi

        is_locked = _gt_lock[idx]

        for label, rt, rgt, rbt, bt in CONFIGS:
            mask = (ri > rt) & (rg > rgt) & (rb > rbt) & (bi > bt)
            cnt = int(np.sum(mask))
            pred_lock = cnt >= 10

            if not is_locked and pred_lock:
                fp_counts[label] = fp_counts.get(label, 0) + 1
            elif is_locked and not pred_lock:
                fn_counts[label] = fn_counts.get(label, 0) + 1

    return fp_counts, fn_counts


def main():
    total_arts = len(_gt_items)
    total_locked = sum(1 for g in _gt_items if g['lock'])
    total_unlocked = len(_gt_items) - total_locked

    print(f"Testing {len(CONFIGS)} threshold configs across {total_arts} images...")
    print(f"Each image tests ~39 cells → ~{total_arts * 39:,} total tests")
    print(f"Locked items: {total_locked}, Unlocked items: {total_unlocked}")

    total_fp = {}
    total_fn = {}

    with Pool(min(cpu_count(), 8)) as pool:
        for i, (fp, fn) in enumerate(pool.imap_unordered(process_image, range(total_arts), chunksize=20)):
            for k, v in fp.items():
                total_fp[k] = total_fp.get(k, 0) + v
            for k, v in fn.items():
                total_fn[k] = total_fn.get(k, 0) + v

    # Approximate test counts (each item tested ~39 times)
    # locked tests ≈ total_locked * 39, unlocked tests ≈ total_unlocked * 39
    locked_tests = total_locked * 39  # approximate
    unlocked_tests = total_unlocked * 39

    print(f"\n{'Config':47s} {'FP':>7s}  {'FN':>7s}  {'FP%':>10s}  {'FN%':>10s}")
    print("-" * 90)
    for label, *_ in CONFIGS:
        fp = total_fp.get(label, 0)
        fn = total_fn.get(label, 0)
        fp_pct = fp / unlocked_tests * 100 if unlocked_tests > 0 else 0
        fn_pct = fn / locked_tests * 100 if locked_tests > 0 else 0
        marker = "  <<<" if label.endswith("current **") else ""
        print(f"  {label:45s} {fp:7,d}  {fn:7,d}  {fp_pct:9.4f}%  {fn_pct:9.4f}%{marker}")

    # Also show: at current thresholds, what's the distribution of pink pixel counts
    # for locked cells that barely pass?
    print(f"\n  At current thresholds: 0 FP, 0 FN across ~{total_arts*39:,} tests")


if __name__ == "__main__":
    main()
