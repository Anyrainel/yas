"""
Detect artifact card edges from full screenshots.

Pixel characteristics (4K resolution):
- Column gaps: ~46px wide, brightness ~57-60 (gray, not black)
- Row gaps: ~17px wide, brightness drops to ~19-50 (very brief, narrow)
- Card backgrounds: brightness ~110 (gold 5-star) or ~85 (purple 4-star)
- Card info/text area (bottom): brightness ~229 (light gray)

GRID_FIRST_X/Y in the code are CLICK targets (card centers), not card edges.

Strategy:
1. COLUMNS: Average horizontal brightness profiles across multiple card-interior
   Y ranges. Column gaps show as clear valleys. Very reliable.
2. ROWS: Use the threshold-based bright-region method from column-sum (averaging
   brightness along X for each Y). Row boundaries are where brightness drops.
3. Compute OX, OY from gap/region spacing. Compute GX, GY as card centers.
"""

import os
import sys
import numpy as np
from PIL import Image
from multiprocessing import Pool, cpu_count
from scipy.signal import find_peaks
from scipy.ndimage import uniform_filter1d
import time

BASE_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"

# Known code constants (at 1080p base)
CODE_GX = 180.0  # card center X
CODE_GY = 253.0  # card center Y
CODE_OX = 145.0  # column spacing
CODE_OY = 166.0  # row spacing
SCALE = 2.0      # 4K = 2x 1080p

CODE_GX_4K = CODE_GX * SCALE
CODE_GY_4K = CODE_GY * SCALE
CODE_OX_4K = CODE_OX * SCALE
CODE_OY_4K = CODE_OY * SCALE

GRID_COLS = 8
GRID_ROWS = 5

# Y ranges for column scanning (inside card artwork regions at 4K)
COLUMN_SCAN_Y_RANGES = [
    (580, 650), (700, 780),    # row 0
    (930, 1000), (1050, 1130), # row 1
    (1280, 1350), (1400, 1480),# row 2
    (1630, 1700), (1750, 1830),# row 3
]

X_MIN = 100
X_MAX = 2600
Y_MIN = 350
Y_MAX = 2050


def get_sample_indices(total=2342, n_samples=20):
    step = total // n_samples
    return [i * step for i in range(n_samples)]


def find_gap_centers(profile, min_distance, n_expected, smooth_width=5):
    """Find gap centers as local minima in brightness profile."""
    smoothed = uniform_filter1d(profile.astype(float), smooth_width)
    neg = -smoothed
    peaks, props = find_peaks(neg, distance=min_distance, prominence=5)
    if len(peaks) > n_expected + 2:
        order = np.argsort(-props['prominences'])
        peaks = np.sort(peaks[order[:n_expected + 2]])
    return peaks, smoothed


def find_edges_at_gaps(profile, gap_centers, search_radius=60):
    """
    For each gap, find precise card_end and next_card_start using gradient.
    Returns: list of (card_end, gap_center, next_card_start) tuples.
    """
    smoothed = uniform_filter1d(profile.astype(float), 3)
    grad = np.diff(smoothed)
    edges = []
    for gc in gap_centers:
        # Card end: steepest fall before gap center
        lo = max(0, gc - search_radius)
        seg = grad[lo:gc]
        left_edge = lo + np.argmin(seg) if len(seg) > 0 else gc
        # Next card start: steepest rise after gap center
        hi = min(len(grad), gc + search_radius)
        seg = grad[gc:hi]
        right_edge = gc + np.argmax(seg) if len(seg) > 0 else gc
        edges.append((int(left_edge), int(gc), int(right_edge)))
    return edges


def find_row_regions(col_sum_profile, y_offset):
    """
    Find card row regions using threshold on column-sum brightness.
    Returns list of (top, bottom, center) for each row region.
    """
    profile = col_sum_profile.copy()
    pmin, pmax = np.percentile(profile, [5, 95])
    if pmax - pmin < 10:
        return []

    threshold = (pmin + pmax) / 2
    above = profile > threshold

    regions = []
    in_card = False
    start = 0
    for i in range(len(above)):
        if above[i] and not in_card:
            start = i
            in_card = True
        elif not above[i] and in_card:
            if i - start > 50:  # min card height ~50px
                regions.append((start + y_offset, i + y_offset,
                               (start + i) // 2 + y_offset))
            in_card = False
    if in_card and len(above) - start > 50:
        regions.append((start + y_offset, len(above) + y_offset,
                       (start + len(above)) // 2 + y_offset))
    return regions


def process_image(scan_idx):
    """Process a single image and detect card boundaries."""
    img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
    if not os.path.exists(img_path):
        return None

    img = np.array(Image.open(img_path).convert('L'))
    h, w = img.shape

    result = {'scan_idx': scan_idx, 'page': scan_idx // 40}

    # === COLUMN DETECTION (horizontal profiles) ===
    col_profiles = []
    for y_lo, y_hi in COLUMN_SCAN_Y_RANGES:
        if y_hi <= h:
            strip = img[y_lo:y_hi, X_MIN:X_MAX].astype(float)
            col_profiles.append(np.mean(strip, axis=0))

    if col_profiles:
        avg_col_profile = np.mean(col_profiles, axis=0)
        col_gap_centers, _ = find_gap_centers(avg_col_profile, min_distance=200, n_expected=9, smooth_width=7)
        col_edges = find_edges_at_gaps(avg_col_profile, col_gap_centers, search_radius=80)
        result['col_gap_centers'] = (col_gap_centers + X_MIN).tolist()
        result['col_edges'] = [(l + X_MIN, c + X_MIN, r + X_MIN) for l, c, r in col_edges]
    else:
        result['col_gap_centers'] = []
        result['col_edges'] = []

    # === ROW DETECTION (column-sum brightness / threshold method) ===
    # Average brightness along X (within grid columns) for each Y position
    col_sum = np.mean(img[Y_MIN:Y_MAX, X_MIN:X_MAX].astype(float), axis=1)
    row_regions = find_row_regions(col_sum, Y_MIN)
    result['row_regions'] = row_regions

    # Also try gap-based detection for rows using the same averaging approach
    # but at specific X positions in card column interiors
    col_centers = [350, 642, 934, 1226, 1518, 1810, 2102, 2394]
    row_profiles = []
    for cx in col_centers:
        x_lo = max(0, cx - 40)
        x_hi = min(w, cx + 40)
        strip = img[Y_MIN:Y_MAX, x_lo:x_hi].astype(float)
        row_profiles.append(np.mean(strip, axis=1))

    if row_profiles:
        avg_row_profile = np.mean(row_profiles, axis=0)
        row_gap_centers, _ = find_gap_centers(avg_row_profile, min_distance=200, n_expected=6, smooth_width=7)
        row_edges = find_edges_at_gaps(avg_row_profile, row_gap_centers, search_radius=80)
        result['row_gap_centers'] = (row_gap_centers + Y_MIN).tolist()
        result['row_edges'] = [(l + Y_MIN, c + Y_MIN, r + Y_MIN) for l, c, r in row_edges]
    else:
        result['row_gap_centers'] = []
        result['row_edges'] = []

    return result


def print_section(title):
    print("\n" + "=" * 80)
    print(title)
    print("=" * 80)


def main():
    t0 = time.time()

    all_dirs = sorted([d for d in os.listdir(BASE_DIR) if os.path.isdir(os.path.join(BASE_DIR, d))])
    total = len(all_dirs)
    print(f"Total artifact directories: {total}")

    sample_indices = get_sample_indices(total, n_samples=20)
    print(f"Sampling {len(sample_indices)} images: {sample_indices}")

    n_workers = min(cpu_count(), 8)
    print(f"Using {n_workers} workers")

    with Pool(n_workers) as pool:
        results = pool.map(process_image, sample_indices)

    results = [r for r in results if r is not None]
    print(f"Processed {len(results)} images in {time.time() - t0:.1f}s")

    # =========================================================================
    print_section("COLUMN GAP DETECTION")
    # =========================================================================

    for r in results:
        idx = r['scan_idx']
        page = r['page']
        edges = r['col_edges']
        print(f"  [{idx:04d}] page={page:3d}: {len(edges)} gaps")
        for left, center, right in edges:
            print(f"         gap@{center}: card_end={left}, card_start={right}, gap={right-left}px")

    # =========================================================================
    print_section("ROW DETECTION (threshold method)")
    # =========================================================================

    for r in results:
        idx = r['scan_idx']
        page = r['page']
        regions = r['row_regions']
        print(f"  [{idx:04d}] page={page:3d}: {len(regions)} rows")
        for top, bot, center in regions:
            print(f"         row: top={top}, bot={bot}, center={center}, height={bot-top}")

    # =========================================================================
    print_section("ROW GAP DETECTION (averaging at column centers)")
    # =========================================================================

    for r in results:
        idx = r['scan_idx']
        page = r['page']
        edges = r['row_edges']
        print(f"  [{idx:04d}] page={page:3d}: {len(edges)} row gaps")
        for left, center, right in edges:
            print(f"         gap@{center}: card_end={left}, card_start={right}, gap={right-left}px")

    # =========================================================================
    print_section("GRID PARAMETER ESTIMATION")
    # =========================================================================

    # --- OX from column gap spacing ---
    print("\n--- OX (column spacing) ---")
    ox_all = []
    for r in results:
        gaps = r['col_gap_centers']
        if len(gaps) >= 7:
            diffs = np.diff(gaps)
            good = [d for d in diffs if 240 < d < 340]
            ox_all.extend(good)

    if ox_all:
        print(f"  OX (4K): mean={np.mean(ox_all):.2f}, std={np.std(ox_all):.2f}, n={len(ox_all)}")
        print(f"  OX (1080p): {np.mean(ox_all)/SCALE:.2f}  (code: {CODE_OX})")
        print(f"  All values (4K): {[f'{v:.0f}' for v in sorted(set(int(x) for x in ox_all))]}")

    # --- OY from row region spacing ---
    print("\n--- OY (row spacing) from threshold regions ---")
    oy_thresh = []
    for r in results:
        regions = r['row_regions']
        if len(regions) == GRID_ROWS:
            centers = [c for t, b, c in regions]
            diffs = np.diff(centers)
            oy_thresh.extend(diffs.tolist())

    if oy_thresh:
        print(f"  OY (4K): mean={np.mean(oy_thresh):.2f}, std={np.std(oy_thresh):.2f}, n={len(oy_thresh)}")
        print(f"  OY (1080p): {np.mean(oy_thresh)/SCALE:.2f}  (code: {CODE_OY})")

    # OY from row gap spacing
    print("\n--- OY (row spacing) from gap detection ---")
    oy_gap = []
    for r in results:
        gaps = r['row_gap_centers']
        if len(gaps) >= 4:
            diffs = np.diff(gaps)
            good = [d for d in diffs if 280 < d < 400]
            oy_gap.extend(good)

    if oy_gap:
        print(f"  OY (4K): mean={np.mean(oy_gap):.2f}, std={np.std(oy_gap):.2f}, n={len(oy_gap)}")
        print(f"  OY (1080p): {np.mean(oy_gap)/SCALE:.2f}  (code: {CODE_OY})")

    # --- Card width and height ---
    print("\n--- Card dimensions ---")
    card_widths = []
    gap_widths = []
    for r in results:
        edges = r['col_edges']
        for left, center, right in edges:
            gw = right - left
            if 30 < gw < 70:  # reasonable gap width
                gap_widths.append(gw)
        # Card width = gap right[i] to gap left[i+1]
        for i in range(len(edges) - 1):
            cw = edges[i + 1][0] - edges[i][2]
            if 200 < cw < 300:
                card_widths.append(cw)

    card_heights = []
    gap_heights_from_regions = []
    for r in results:
        regions = r['row_regions']
        for t, b, c in regions:
            h = b - t
            if 50 < h < 400:
                card_heights.append(h)
        # Row gap = bottom of one region to top of next
        for i in range(len(regions) - 1):
            gh = regions[i + 1][0] - regions[i][1]
            if gh > 0:
                gap_heights_from_regions.append(gh)

    if card_widths:
        print(f"  Card width (4K): mean={np.mean(card_widths):.1f}, std={np.std(card_widths):.1f}")
        print(f"  Card width (1080p): {np.mean(card_widths)/SCALE:.1f}")
    if card_heights:
        print(f"  Card height (4K): mean={np.mean(card_heights):.1f}, std={np.std(card_heights):.1f}")
        print(f"  Card height (1080p): {np.mean(card_heights)/SCALE:.1f}")
    if gap_widths:
        print(f"  Column gap (4K): mean={np.mean(gap_widths):.1f}, std={np.std(gap_widths):.1f}")
        print(f"  Column gap (1080p): {np.mean(gap_widths)/SCALE:.1f}")
    if gap_heights_from_regions:
        print(f"  Row gap (4K, from regions): mean={np.mean(gap_heights_from_regions):.1f}, "
              f"std={np.std(gap_heights_from_regions):.1f}")
        print(f"  Row gap (1080p): {np.mean(gap_heights_from_regions)/SCALE:.1f}")

    # --- GX, GY as card CENTER positions ---
    print("\n--- GX, GY (card center positions, for clicking) ---")
    print("  (Code uses card centers as click targets)")

    # GX: card center = card left + card_width/2
    # Card left[i] = col_edges[i-1].right (next card start after gap)
    # For the first card, card_left = col_edges[0].right - OX
    gx_center_values = []
    for r in results:
        edges = r['col_edges']
        if len(edges) >= 7 and card_widths:
            half_w = np.mean(card_widths) / 2
            # Find the first INTERNAL gap (not boundary gap)
            # Internal gaps have gap width ~46px. Boundary gaps are wider.
            internal_edges = [(l, c, ri) for l, c, ri in edges if 30 < ri - l < 60]
            if len(internal_edges) >= 7:
                # Card left positions from internal gaps
                # First internal gap separates card 0 from card 1
                first_card_start = internal_edges[0][2]  # start of card 1
                # Card 0 left = first_card_start - OX
                if ox_all:
                    ox_mean = np.mean(ox_all)
                    card0_left = first_card_start - ox_mean
                    card0_center = card0_left + half_w
                    gx_center_values.append(card0_center)

    gy_center_values = []
    for r in results:
        regions = r['row_regions']
        if len(regions) == GRID_ROWS:
            # Row 0 center
            gy_center_values.append(regions[0][2])  # center of first row

    if gx_center_values:
        print(f"  GX (4K, card center): mean={np.mean(gx_center_values):.1f}, "
              f"std={np.std(gx_center_values):.1f}")
        print(f"  GX (1080p): {np.mean(gx_center_values)/SCALE:.1f}  (code: {CODE_GX})")

    if gy_center_values:
        print(f"  GY (4K, card center): mean={np.mean(gy_center_values):.1f}, "
              f"std={np.std(gy_center_values):.1f}")
        print(f"  GY (1080p): {np.mean(gy_center_values)/SCALE:.1f}  (code: {CODE_GY})")

    # =========================================================================
    print_section("PER-PAGE VARIATION")
    # =========================================================================

    for r in results:
        idx = r['scan_idx']
        page = r['page']
        col_edges = r['col_edges']
        regions = r['row_regions']

        ox_str = ""
        gx_str = ""
        if len(col_edges) >= 7:
            internal = [(l, c, ri) for l, c, ri in col_edges if 30 < ri - l < 60]
            if len(internal) >= 7:
                gaps = [c for l, c, ri in internal]
                diffs = np.diff(gaps)
                good = [d for d in diffs if 240 < d < 340]
                if good:
                    ox_str = f"OX={np.mean(good):.0f}"
                    # GX as card center
                    if card_widths and ox_all:
                        half_w = np.mean(card_widths) / 2
                        card0_left = internal[0][2] - np.mean(ox_all)
                        gx_c = card0_left + half_w
                        gx_str = f"GX={gx_c:.0f}({gx_c/SCALE:.0f}@1080)"

        oy_str = ""
        gy_str = ""
        if len(regions) == GRID_ROWS:
            centers = [c for t, b, c in regions]
            diffs = np.diff(centers)
            oy_str = f"OY={np.mean(diffs):.0f}"
            gy_str = f"GY={centers[0]}({centers[0]/SCALE:.0f}@1080)"

        print(f"  [{idx:04d}] page={page:3d}: "
              f"cols={len(col_edges):2d}  rows={len(regions):1d}  "
              f"{ox_str:10s} {gx_str:25s}  {oy_str:10s} {gy_str}")

    # =========================================================================
    print_section("CELL POSITION TABLE")
    # =========================================================================

    for r in results:
        col_edges = r['col_edges']
        regions = r['row_regions']

        internal = [(l, c, ri) for l, c, ri in col_edges if 30 < ri - l < 60]
        if len(internal) >= 7 and len(regions) == GRID_ROWS and ox_all and card_widths:
            idx = r['scan_idx']
            page = r['page']
            ox_mean = np.mean(ox_all)
            half_w = np.mean(card_widths) / 2

            # Card center X positions
            card0_left = internal[0][2] - ox_mean
            card_centers_x = [card0_left + half_w + i * ox_mean for i in range(GRID_COLS)]

            # Card center Y positions
            card_centers_y = [c for t, b, c in regions]

            print(f"\n  Image [{idx:04d}] (page {page})")
            print(f"  Card center X (4K): {[f'{x:.0f}' for x in card_centers_x]}")
            print(f"  Card center Y (4K): {card_centers_y}")

            # Offsets
            ox_actual = [card_centers_x[i+1] - card_centers_x[i] for i in range(GRID_COLS-1)]
            oy_actual = [card_centers_y[i+1] - card_centers_y[i] for i in range(GRID_ROWS-1)]
            print(f"  OX: {[f'{x:.0f}' for x in ox_actual]} mean={np.mean(ox_actual):.1f}")
            print(f"  OY: {oy_actual} mean={np.mean(oy_actual):.1f}")

            print(f"\n  {'Cell':>8s}  {'CtrX':>6s}  {'CtrY':>6s}  "
                  f"{'CodeX':>6s}  {'CodeY':>6s}  {'dX':>6s}  {'dY':>6s}")
            print(f"  {'----':>8s}  {'----':>6s}  {'----':>6s}  "
                  f"{'-----':>6s}  {'-----':>6s}  {'---':>6s}  {'---':>6s}")

            for row_i in range(GRID_ROWS):
                for col_i in range(GRID_COLS):
                    cx = card_centers_x[col_i]
                    cy = card_centers_y[row_i]
                    code_x = CODE_GX_4K + col_i * CODE_OX_4K
                    code_y = CODE_GY_4K + row_i * CODE_OY_4K
                    dx = cx - code_x
                    dy = cy - code_y
                    print(f"  ({row_i},{col_i})   {cx:6.0f}  {cy:6d}  "
                          f"{code_x:6.0f}  {code_y:6.0f}  {dx:+6.0f}  {dy:+6.0f}")

            break

    # =========================================================================
    print_section("SUMMARY")
    # =========================================================================

    print(f"\n  Code constants (1080p): GX={CODE_GX}, GY={CODE_GY}, OX={CODE_OX}, OY={CODE_OY}")
    print(f"  (GX/GY = card center click positions, OX/OY = card-to-card spacing)")
    print()

    if ox_all:
        v = np.mean(ox_all) / SCALE
        print(f"  Measured OX (1080p): {v:.2f}  code={CODE_OX}  delta={v - CODE_OX:+.2f}")
    if oy_thresh:
        v = np.mean(oy_thresh) / SCALE
        print(f"  Measured OY (1080p): {v:.2f}  code={CODE_OY}  delta={v - CODE_OY:+.2f}  (threshold method)")
    if oy_gap:
        v = np.mean(oy_gap) / SCALE
        print(f"  Measured OY (1080p): {v:.2f}  code={CODE_OY}  delta={v - CODE_OY:+.2f}  (gap method)")
    if gx_center_values:
        v = np.mean(gx_center_values) / SCALE
        print(f"  Measured GX (1080p): {v:.2f}  code={CODE_GX}  delta={v - CODE_GX:+.2f}  (card center)")
    if gy_center_values:
        v = np.mean(gy_center_values) / SCALE
        print(f"  Measured GY (1080p): {v:.2f}  code={CODE_GY}  delta={v - CODE_GY:+.2f}  (card center)")

    print()
    if card_widths:
        print(f"  Card width (1080p):    {np.mean(card_widths)/SCALE:.1f}")
    if card_heights:
        print(f"  Card height (1080p):   {np.mean(card_heights)/SCALE:.1f}")
    if gap_widths:
        print(f"  Column gap (1080p):    {np.mean(gap_widths)/SCALE:.1f}")
    if gap_heights_from_regions:
        print(f"  Row gap (1080p):       {np.mean(gap_heights_from_regions)/SCALE:.1f}")

    # Verification
    if card_widths and gap_widths and ox_all:
        computed = (np.mean(card_widths) + np.mean(gap_widths)) / SCALE
        measured = np.mean(ox_all) / SCALE
        print(f"\n  Check: card_w + col_gap = {computed:.1f}, measured OX = {measured:.1f}")
    if card_heights and gap_heights_from_regions and oy_thresh:
        computed = (np.mean(card_heights) + np.mean(gap_heights_from_regions)) / SCALE
        measured = np.mean(oy_thresh) / SCALE
        print(f"  Check: card_h + row_gap = {computed:.1f}, measured OY = {measured:.1f}")

    # Page stability analysis
    print()
    gx_per_page = []
    gy_per_page = []
    for r in results:
        edges = r['col_edges']
        regions = r['row_regions']
        internal = [(l, c, ri) for l, c, ri in edges if 30 < ri - l < 60]
        if len(internal) >= 7 and card_widths and ox_all:
            half_w = np.mean(card_widths) / 2
            card0_left = internal[0][2] - np.mean(ox_all)
            gx_per_page.append((r['page'], card0_left + half_w))
        if len(regions) == GRID_ROWS:
            gy_per_page.append((r['page'], regions[0][2]))

    if gx_per_page:
        gx_vals = [v for p, v in gx_per_page]
        print(f"  GX stability across pages: mean={np.mean(gx_vals)/SCALE:.1f}@1080, "
              f"std={np.std(gx_vals)/SCALE:.2f}, "
              f"range=[{np.min(gx_vals)/SCALE:.1f}, {np.max(gx_vals)/SCALE:.1f}]")
    if gy_per_page:
        gy_vals = [v for p, v in gy_per_page]
        print(f"  GY stability across pages: mean={np.mean(gy_vals)/SCALE:.1f}@1080, "
              f"std={np.std(gy_vals)/SCALE:.2f}, "
              f"range=[{np.min(gy_vals)/SCALE:.1f}, {np.max(gy_vals)/SCALE:.1f}]")

        # Check if there are two distinct groups (page 0 vs rest)
        if len(gy_vals) > 2:
            sorted_gy = sorted(gy_vals)
            # Check if page 0 differs from the rest
            page0_gy = [v for p, v in gy_per_page if p == 0]
            rest_gy = [v for p, v in gy_per_page if p > 0]
            if page0_gy and rest_gy:
                print(f"  GY page 0: {page0_gy[0]/SCALE:.1f}@1080")
                print(f"  GY other pages: mean={np.mean(rest_gy)/SCALE:.1f}@1080, "
                      f"std={np.std(rest_gy)/SCALE:.2f}")

    print(f"\n  Total elapsed: {time.time() - t0:.1f}s")


if __name__ == '__main__':
    main()
