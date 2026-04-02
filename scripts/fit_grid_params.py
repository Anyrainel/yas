"""
Back-calculate optimal grid parameters (GX, GY, OX, OY) and lock icon offset
from pink lock centroid measurements across all full.png screenshots.

Uses multiprocessing (8 workers) and numpy vectorized pink detection.
"""

import json
import os
import sys
import time
from multiprocessing import Pool
from pathlib import Path

import numpy as np
from PIL import Image

# --- Constants ---
DEBUG_DIR = Path("F:/Codes/genshin/yas/target/release/debug_images/artifacts")
GT_PATH = Path("F:/Codes/genshin/yas/scripts/grid_ground_truth.json")
OUTPUT_PATH = Path("F:/Codes/genshin/yas/scripts/grid_fit_results.json")

# Current grid params at 4K (2x of 1080p)
GX_INIT, GY_INIT = 360, 506
OX_INIT, OY_INIT = 290, 332
LOCK_DX_INIT, LOCK_DY_INIT = -89, -112

COLS_PER_ROW = 8
ROWS_PER_PAGE = 5
ITEMS_PER_PAGE = COLS_PER_ROW * ROWS_PER_PAGE

# Pink detection thresholds
PINK_R_MIN = 180
PINK_RG_DIFF = 60
PINK_RB_DIFF = 50
PINK_B_MIN = 70
PINK_MIN_COUNT = 10

# Search window around expected lock position (at 4K)
SEARCH_HALF_W = 80
SEARCH_HALF_H = 70


def find_pink_centroid(img_array, cx, cy):
    """Find pink lock centroid in a search window around (cx, cy).
    Uses numpy vectorized operations. Returns (centroid_x, centroid_y) or None.
    """
    h, w = img_array.shape[:2]
    x0 = max(0, cx - SEARCH_HALF_W)
    x1 = min(w, cx + SEARCH_HALF_W)
    y0 = max(0, cy - SEARCH_HALF_H)
    y1 = min(h, cy + SEARCH_HALF_H)

    patch = img_array[y0:y1, x0:x1]
    r = patch[:, :, 0].astype(np.int16)
    g = patch[:, :, 1].astype(np.int16)
    b = patch[:, :, 2].astype(np.int16)

    mask = (r > PINK_R_MIN) & ((r - g) > PINK_RG_DIFF) & ((r - b) > PINK_RB_DIFF) & (b > PINK_B_MIN)
    count = np.sum(mask)
    if count < PINK_MIN_COUNT:
        return None

    ys, xs = np.where(mask)
    centroid_x = np.mean(xs) + x0
    centroid_y = np.mean(ys) + y0
    return (centroid_x, centroid_y)


def process_image(args):
    """Process a single full.png: find pink centroids for all locked items on this page.
    Returns list of (scan_idx, item_idx, row, col, centroid_x, centroid_y).
    """
    scan_idx, locked_indices = args
    img_path = DEBUG_DIR / f"{scan_idx:04d}" / "full.png"
    if not img_path.exists():
        return []

    img = Image.open(img_path)
    img_array = np.array(img)

    page = scan_idx // ITEMS_PER_PAGE
    results = []

    for item_idx in locked_indices:
        if item_idx // ITEMS_PER_PAGE != page:
            continue
        pos = item_idx % ITEMS_PER_PAGE
        row = pos // COLS_PER_ROW
        col = pos % COLS_PER_ROW

        # Expected lock position using initial estimates
        expected_x = GX_INIT + col * OX_INIT + LOCK_DX_INIT
        expected_y = GY_INIT + row * OY_INIT + LOCK_DY_INIT

        centroid = find_pink_centroid(img_array, int(expected_x), int(expected_y))
        if centroid is not None:
            results.append((scan_idx, item_idx, row, col, centroid[0], centroid[1]))

    return results


def main():
    t0 = time.time()

    # Load ground truth
    with open(GT_PATH) as f:
        gt = json.load(f)

    items = gt["items"]
    total_items = gt["total_items"]

    # Build set of locked item indices
    locked_set = set()
    for item in items:
        if item["lock"]:
            locked_set.add(item["idx"])

    print(f"Total items: {total_items}, locked: {len(locked_set)}")

    # Group locked items by page, and map scan_idx to page
    # Each scan_idx is on page = scan_idx // 40
    # We process each scan_idx that has at least one locked item on its page
    pages = {}
    for idx in locked_set:
        page = idx // ITEMS_PER_PAGE
        pages.setdefault(page, []).append(idx)

    # For each page, we only need to open one full.png per locked item
    # But full.png is per scan_idx (each item has its own screenshot of the full page)
    # We can pick any scan_idx on that page - let's use the first locked one per page
    # Actually, each scan_idx screenshot shows the SAME page, so we pick one per page
    # to avoid redundant work. But different scans of same page should give same result.
    # Let's use the first item on each page as the representative scan.
    tasks = []
    for page, idxs in sorted(pages.items()):
        # Use the first item on this page as the scan to open
        first_idx_on_page = page * ITEMS_PER_PAGE
        # But we need an actual scan_idx that exists
        # The scan_idx = item_idx for the screenshots
        representative = min(idxs)
        tasks.append((representative, idxs))

    print(f"Pages with locked items: {len(tasks)}")
    print(f"Processing with 8 workers...")

    # Run multiprocessing
    all_centroids = []
    with Pool(8) as pool:
        for batch_results in pool.imap_unordered(process_image, tasks):
            all_centroids.extend(batch_results)

    print(f"Found {len(all_centroids)} pink centroids in {time.time()-t0:.1f}s")

    if len(all_centroids) == 0:
        print("ERROR: No centroids found!")
        return

    # Convert to numpy arrays
    data = np.array(all_centroids)
    scan_idxs = data[:, 0].astype(int)
    item_idxs = data[:, 1].astype(int)
    rows = data[:, 2].astype(int)
    cols = data[:, 3].astype(int)
    cx = data[:, 4]  # centroid x
    cy = data[:, 5]  # centroid y

    pages = scan_idxs // ITEMS_PER_PAGE
    unique_pages = np.unique(pages)
    n_pages = len(unique_pages)
    print(f"Data spans {n_pages} pages: {unique_pages.tolist()}")

    # --- Fit X parameters: cx = GX + col*OX + lock_dx ---
    # Since GX and lock_dx are not separable (both are constants), we fit:
    #   cx = (GX + lock_dx) + col*OX + per_page_offset_x[page]
    # Let A_x = GX + lock_dx (combined X intercept)
    #
    # Check if per-page X offset is needed by first fitting without it

    # Simple fit: cx = A_x + col * OX
    # Using least squares: [1, col] @ [A_x, OX]^T = cx
    X_design_simple = np.column_stack([np.ones(len(cx)), cols.astype(float)])
    params_x_simple, res_x_simple, _, _ = np.linalg.lstsq(X_design_simple, cx, rcond=None)
    A_x_simple, OX_fit_simple = params_x_simple
    residuals_x_simple = cx - X_design_simple @ params_x_simple

    # Fit with per-page X offsets
    # cx = A_x + col*OX + sum(page_offset_x[p] * (page==p))
    # Columns: [1, col, indicator_page_0, indicator_page_1, ...]
    page_to_idx = {p: i for i, p in enumerate(unique_pages)}
    page_indicators = np.zeros((len(cx), n_pages))
    for i, p in enumerate(pages):
        page_indicators[i, page_to_idx[p]] = 1.0

    # To avoid rank deficiency (constant col already has intercept),
    # we drop one page indicator (first page = reference)
    X_design_x_full = np.column_stack([np.ones(len(cx)), cols.astype(float), page_indicators[:, 1:]])
    params_x_full, res_x_full, _, _ = np.linalg.lstsq(X_design_x_full, cx, rcond=None)
    residuals_x_full = cx - X_design_x_full @ params_x_full

    std_simple_x = np.std(residuals_x_simple)
    std_full_x = np.std(residuals_x_full)
    print(f"\nX residuals - simple: std={std_simple_x:.2f}, with page offsets: std={std_full_x:.2f}")
    x_needs_page_offsets = (std_simple_x - std_full_x) > 0.5

    # --- Fit Y parameters: cy = GY + row*OY + lock_dy + page_scroll_offset[page] ---
    # Combined: cy = (GY + lock_dy) + row*OY + page_scroll[page]
    # Let A_y = GY + lock_dy
    # Columns: [1, row, page_indicators (drop first)]
    X_design_y = np.column_stack([np.ones(len(cy)), rows.astype(float), page_indicators[:, 1:]])
    params_y, _, _, _ = np.linalg.lstsq(X_design_y, cy, rcond=None)
    A_y = params_y[0]
    OY_fit = params_y[1]
    page_scroll_offsets_y = np.zeros(n_pages)
    page_scroll_offsets_y[1:] = params_y[2:]  # first page is reference (offset=0)

    residuals_y = cy - X_design_y @ params_y

    # Also fit Y without page offsets for comparison
    X_design_y_simple = np.column_stack([np.ones(len(cy)), rows.astype(float)])
    params_y_simple, _, _, _ = np.linalg.lstsq(X_design_y_simple, cy, rcond=None)
    residuals_y_simple = cy - X_design_y_simple @ params_y_simple

    print(f"Y residuals - simple: std={np.std(residuals_y_simple):.2f}, with page offsets: std={np.std(residuals_y):.2f}")

    # Use simple X fit (no page offsets) since X shouldn't have scroll
    OX_fit = params_x_simple[1]
    A_x = params_x_simple[0]

    # --- Decompose A_x and A_y into grid origin + lock offset ---
    # We can't separate GX from lock_dx with centroid data alone.
    # But we know the current estimates. Let's report the combined values
    # and also try to decompose using the constraint that GX, GY should be
    # close to integer cell centers.
    #
    # Strategy: lock_dx and lock_dy should be the same for all items.
    # GX + lock_dx = A_x, so if we pick lock_dx, GX = A_x - lock_dx
    # The "natural" decomposition: GX is the cell center X for col=0,
    # lock_dx is the offset from cell center to lock icon.
    # We'll report A_x as the combined value and note they're not separable from this data alone.

    # --- Results ---
    print("\n" + "="*60)
    print("GRID PARAMETER FIT RESULTS")
    print("="*60)

    print(f"\n--- X axis ---")
    print(f"  OX (col spacing):     {OX_fit:.2f} px (4K)  /  {OX_fit/2:.2f} px (1080p)")
    print(f"  GX + lock_dx:         {A_x:.2f} px (4K)  /  {A_x/2:.2f} px (1080p)")
    print(f"  Current OX:           {OX_INIT} px (4K)")
    print(f"  Current GX+lock_dx:   {GX_INIT + LOCK_DX_INIT} px (4K)")
    print(f"  X needs page offsets: {x_needs_page_offsets}")

    print(f"\n--- Y axis ---")
    print(f"  OY (row spacing):     {OY_fit:.2f} px (4K)  /  {OY_fit/2:.2f} px (1080p)")
    print(f"  GY + lock_dy:         {A_y:.2f} px (4K)  /  {A_y/2:.2f} px (1080p)")
    print(f"  Current OY:           {OY_INIT} px (4K)")
    print(f"  Current GY+lock_dy:   {GY_INIT + LOCK_DY_INIT} px (4K)")

    # Decompose using current lock offset as starting point
    # Since we can fit OX, OY precisely, we can refine lock_dx, lock_dy
    # by assuming GX, GY are the "round" values.
    # Best approach: try a few GX candidates near A_x and see which gives
    # a lock_dx that's consistent.
    # For now, report with current GX/GY as anchors:
    lock_dx_fit = A_x - GX_INIT
    lock_dy_fit = A_y - GY_INIT
    print(f"\n--- Lock offset (assuming GX={GX_INIT}, GY={GY_INIT}) ---")
    print(f"  lock_dx:              {lock_dx_fit:.2f} px (4K)  /  {lock_dx_fit/2:.2f} px (1080p)")
    print(f"  lock_dy:              {lock_dy_fit:.2f} px (4K)  /  {lock_dy_fit/2:.2f} px (1080p)")
    print(f"  Current lock_dx:      {LOCK_DX_INIT} px (4K)")
    print(f"  Current lock_dy:      {LOCK_DY_INIT} px (4K)")

    # Alternative: find GX that makes lock_dx closest to integer
    # Try GX values near current
    best_gx = None
    best_score = 999
    for gx_candidate in range(GX_INIT - 20, GX_INIT + 20):
        ldx = A_x - gx_candidate
        score = abs(ldx - round(ldx))
        if score < best_score:
            best_score = score
            best_gx = gx_candidate

    best_gy = None
    best_score_y = 999
    for gy_candidate in range(GY_INIT - 20, GY_INIT + 20):
        ldy = A_y - gy_candidate
        score = abs(ldy - round(ldy))
        if score < best_score_y:
            best_score_y = score
            best_gy = gy_candidate

    lock_dx_int = round(A_x - best_gx)
    lock_dy_int = round(A_y - best_gy)
    print(f"\n--- Best integer decomposition ---")
    print(f"  GX={best_gx}, lock_dx={lock_dx_int} (sum={best_gx+lock_dx_int}, actual A_x={A_x:.2f})")
    print(f"  GY={best_gy}, lock_dy={lock_dy_int} (sum={best_gy+lock_dy_int}, actual A_y={A_y:.2f})")

    print(f"\n--- Residual errors (after fitting) ---")
    print(f"  X: mean={np.mean(residuals_x_simple):.3f}, std={np.std(residuals_x_simple):.3f}, max={np.max(np.abs(residuals_x_simple)):.3f}")
    print(f"  Y: mean={np.mean(residuals_y):.3f}, std={np.std(residuals_y):.3f}, max={np.max(np.abs(residuals_y)):.3f}")
    print(f"  Y (no page offsets): mean={np.mean(residuals_y_simple):.3f}, std={np.std(residuals_y_simple):.3f}, max={np.max(np.abs(residuals_y_simple)):.3f}")

    print(f"\n--- Per-page Y scroll offsets (relative to page {unique_pages[0]}) ---")
    page_offset_dict = {}
    for i, p in enumerate(unique_pages):
        offset = page_scroll_offsets_y[i]
        page_offset_dict[int(p)] = round(float(offset), 2)
        print(f"  Page {p:2d}: {offset:+8.2f} px")

    # Check per-page X offsets too
    if x_needs_page_offsets:
        page_offsets_x = np.zeros(n_pages)
        page_offsets_x[1:] = params_x_full[2:]
        print(f"\n--- Per-page X offsets (significant) ---")
        for i, p in enumerate(unique_pages):
            print(f"  Page {p:2d}: {page_offsets_x[i]:+8.2f} px")
    else:
        print(f"\n  X per-page offsets are negligible (as expected for horizontal axis)")

    # --- Per-row residual analysis ---
    print(f"\n--- Per-row Y residual (with page offsets) ---")
    for r in range(ROWS_PER_PAGE):
        mask = rows == r
        if np.any(mask):
            res_r = residuals_y[mask]
            print(f"  Row {r}: n={np.sum(mask):4d}, mean={np.mean(res_r):+.3f}, std={np.std(res_r):.3f}")

    print(f"\n--- Per-col X residual ---")
    for c in range(COLS_PER_ROW):
        mask = cols == c
        if np.any(mask):
            res_c = residuals_x_simple[mask]
            print(f"  Col {c}: n={np.sum(mask):4d}, mean={np.mean(res_c):+.3f}, std={np.std(res_c):.3f}")

    # --- Save results ---
    results = {
        "description": "Best-fit grid parameters from pink lock centroid regression",
        "n_centroids": len(all_centroids),
        "n_pages": n_pages,
        "fit_4k": {
            "OX": round(float(OX_fit), 3),
            "OY": round(float(OY_fit), 3),
            "GX_plus_lock_dx": round(float(A_x), 3),
            "GY_plus_lock_dy": round(float(A_y), 3),
            "lock_dx_assuming_GX_360": round(float(lock_dx_fit), 3),
            "lock_dy_assuming_GY_506": round(float(lock_dy_fit), 3),
            "best_integer_decomposition": {
                "GX": best_gx,
                "GY": best_gy,
                "lock_dx": lock_dx_int,
                "lock_dy": lock_dy_int,
            },
        },
        "fit_1080p": {
            "OX": round(float(OX_fit / 2), 3),
            "OY": round(float(OY_fit / 2), 3),
            "GX_plus_lock_dx": round(float(A_x / 2), 3),
            "GY_plus_lock_dy": round(float(A_y / 2), 3),
        },
        "current_4k": {
            "GX": GX_INIT,
            "GY": GY_INIT,
            "OX": OX_INIT,
            "OY": OY_INIT,
            "lock_dx": LOCK_DX_INIT,
            "lock_dy": LOCK_DY_INIT,
        },
        "residuals": {
            "x": {
                "mean": round(float(np.mean(residuals_x_simple)), 4),
                "std": round(float(np.std(residuals_x_simple)), 4),
                "max_abs": round(float(np.max(np.abs(residuals_x_simple))), 4),
            },
            "y_with_page_offsets": {
                "mean": round(float(np.mean(residuals_y)), 4),
                "std": round(float(np.std(residuals_y)), 4),
                "max_abs": round(float(np.max(np.abs(residuals_y))), 4),
            },
            "y_without_page_offsets": {
                "mean": round(float(np.mean(residuals_y_simple)), 4),
                "std": round(float(np.std(residuals_y_simple)), 4),
                "max_abs": round(float(np.max(np.abs(residuals_y_simple))), 4),
            },
        },
        "x_needs_page_offsets": bool(x_needs_page_offsets),
        "per_page_y_scroll_offsets": page_offset_dict,
    }

    with open(OUTPUT_PATH, "w") as f:
        json.dump(results, f, indent=2)

    print(f"\nResults saved to {OUTPUT_PATH}")
    print(f"Total time: {time.time()-t0:.1f}s")


if __name__ == "__main__":
    main()
