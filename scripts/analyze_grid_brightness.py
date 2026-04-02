"""Offline brightness analysis of selection grid debug images.

Reads *_raw.png images from debug_images/set_filter_test/ and computes
mean brightness at each grid cell position. Prints a table for threshold
calibration.

Usage:
    python scripts/analyze_grid_brightness.py
    python scripts/analyze_grid_brightness.py debug_images/set_filter_test/002_page4_raw.png
"""

import sys
import os
from pathlib import Path

import numpy as np
from PIL import Image

# Selection grid constants (at 1920x1080 — images are downscaled to this)
SEL_COLS = 4
SEL_ROWS = 5
SEL_FIRST_X = 89.0
SEL_FIRST_Y = 130.0
SEL_OFFSET_X = 141.0
SEL_OFFSET_Y = 167.0
SAMPLE_HALF = 20  # pixels at 1080p


def analyze_image(path: str, threshold: float = 38.0):
    img = Image.open(path).convert("RGB")
    arr = np.array(img, dtype=np.float64)
    w, h = img.size
    scale = w / 1920.0

    print(f"\n{'='*70}")
    print(f"File: {os.path.basename(path)}  ({w}x{h}, scale={scale:.1f})")
    print(f"{'='*70}")

    # Card dimensions from grid_icon_detector.rs (1080p base)
    CARD_H_HALF = 76.5  # 153/2

    # Multiple sampling strategies to find which works best
    strategies = {
        "center":    (0,    0,    20),   # (dy_offset, sample_half) — current
        "star_bar":  (0,    65,   8),    # bottom of card: rarity stars
        "top_edge":  (0,   -70,   8),    # top edge of card
        "variance":  (0,    0,    30),   # larger area for variance calc
    }

    for name, (dx_off, dy_off, s_half) in strategies.items():
        print(f"\n--- Strategy: {name} (dy={dy_off:+d}, half={s_half}) ---")
        hw = int(s_half * scale)
        hh = int(s_half * scale)

        results = []
        header = "     " + "".join(f"  col{c}  " for c in range(SEL_COLS))
        print(header)

        for row in range(SEL_ROWS):
            row_str = f"r{row}  "
            for col in range(SEL_COLS):
                cx = int((SEL_FIRST_X + col * SEL_OFFSET_X + dx_off) * scale)
                cy = int((SEL_FIRST_Y + row * SEL_OFFSET_Y + dy_off) * scale)

                x1 = max(0, cx - hw)
                y1 = max(0, cy - hh)
                x2 = min(w, cx + hw)
                y2 = min(h, cy + hh)

                crop = arr[y1:y2, x1:x2]

                if name == "variance":
                    # Use brightness standard deviation
                    gray = crop.mean(axis=2)
                    val = gray.std()
                else:
                    val = crop.mean()

                results.append((row, col, val))
                row_str += f" {val:6.1f} "
            print(row_str)

        vals = [v for _, _, v in results]
        print(f"  Range: {min(vals):.1f} - {max(vals):.1f}")

    return []


def main():
    if len(sys.argv) > 1:
        # Analyze specific files
        for path in sys.argv[1:]:
            analyze_image(path)
    else:
        # Analyze all raw images in debug dir
        debug_dir = Path("debug_images/set_filter_test")
        raw_files = sorted(debug_dir.glob("*_raw.png"))
        if not raw_files:
            print(f"No *_raw.png files found in {debug_dir}")
            return

        all_occupied = []
        all_empty = []

        for path in raw_files:
            results = analyze_image(str(path))
            for _, _, b, o in results:
                if o:
                    all_occupied.append(b)
                else:
                    all_empty.append(b)

        print(f"\n{'='*60}")
        print(f"SUMMARY across {len(raw_files)} images")
        print(f"{'='*60}")
        print(f"Occupied: {len(all_occupied)} cells, "
              f"range: {min(all_occupied):.1f} - {max(all_occupied):.1f}, "
              f"mean: {np.mean(all_occupied):.1f}" if all_occupied else "")
        print(f"Empty:    {len(all_empty)} cells, "
              f"range: {min(all_empty):.1f} - {max(all_empty):.1f}, "
              f"mean: {np.mean(all_empty):.1f}" if all_empty else "")
        if all_occupied and all_empty:
            gap = min(all_occupied) - max(all_empty)
            midpoint = (min(all_occupied) + max(all_empty)) / 2
            print(f"Gap: {gap:.1f}")
            print(f"Suggested threshold: {midpoint:.1f}")


if __name__ == "__main__":
    main()
