"""
Refine card center position by measuring actual card edges.

Uses column gap detection (reliable) and row gap detection
to precisely locate card boundaries and compute true GX, GY.
Also measures card width/height.
"""

import json
import os
import numpy as np
from PIL import Image
from multiprocessing import Pool, cpu_count

BASE_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
FIT_FILE = "F:/Codes/genshin/yas/scripts/grid_fit_results.json"

with open(FIT_FILE) as f:
    fit = json.load(f)
NEW_OX = fit['fit_4k']['OX']  # 292.5
NEW_OY = fit['fit_4k']['OY']  # 349.4
page_offsets = {int(k): v for k, v in fit['per_page_y_scroll_offsets'].items()}


def detect_column_gaps(img_np):
    """Find column gap positions using vertical brightness gradient."""
    h, w = img_np.shape[:2]
    # Sum brightness in a horizontal band through the middle of the grid
    # Use rows 400-1600 at 4K to cover most of the grid
    band = img_np[400:1600, :, :3].astype(np.float32)
    col_brightness = band.mean(axis=(0, 2))  # shape: (w,)

    # Smooth slightly
    kernel = np.ones(5) / 5
    col_smooth = np.convolve(col_brightness, kernel, mode='same')

    # Find dips (gaps are darker than cards)
    # Look for local minima in the grid region (x: 200-2600 at 4K)
    grad = np.diff(col_smooth)

    # Find zero crossings of gradient (neg→pos = local minimum)
    gaps = []
    for x in range(250, 2550):
        if grad[x-1] < -1 and grad[x] > 1:  # neg→pos crossing
            gaps.append(x)

    # Filter: gaps should be ~292px apart
    if len(gaps) < 2:
        return None

    # Cluster nearby detections
    clustered = [gaps[0]]
    for g in gaps[1:]:
        if g - clustered[-1] > 50:
            clustered.append(g)
        else:
            clustered[-1] = (clustered[-1] + g) // 2

    return clustered


def detect_row_edges(img_np, col_centers):
    """Find row boundaries using horizontal brightness profiles at card centers."""
    h, w = img_np.shape[:2]

    # For each column center, take a vertical brightness profile
    profiles = []
    for cx in col_centers[:4]:  # Use first 4 columns
        x1 = max(0, int(cx) - 20)
        x2 = min(w, int(cx) + 20)
        strip = img_np[:, x1:x2, :3].astype(np.float32)
        prof = strip.mean(axis=(1, 2))
        profiles.append(prof)

    # Average the profiles for stability
    avg_profile = np.mean(profiles, axis=0)

    # Smooth
    kernel = np.ones(5) / 5
    smooth = np.convolve(avg_profile, kernel, mode='same')

    # The profile should show: bright card → dark gap → bright card → ...
    # Find edges using gradient
    grad = np.diff(smooth)

    # Find strong negative edges (card→gap, brightness drops)
    # and strong positive edges (gap→card, brightness rises)
    top_edges = []  # gap→card transitions (start of card)
    bot_edges = []  # card→gap transitions (end of card)

    # Look in the grid region (y: 150-1900 at 4K)
    for y in range(200, 1850):
        if grad[y] > 3 and smooth[y+1] > smooth[y-5] + 10:
            top_edges.append(y)
        if grad[y] < -3 and smooth[y+1] < smooth[y-5] - 10:
            bot_edges.append(y)

    # Cluster
    def cluster(edges, min_gap=50):
        if not edges:
            return []
        result = [edges[0]]
        for e in edges[1:]:
            if e - result[-1] > min_gap:
                result.append(e)
            else:
                result[-1] = (result[-1] + e) // 2
        return result

    top_edges = cluster(top_edges)
    bot_edges = cluster(bot_edges)

    return top_edges, bot_edges, avg_profile


def process_image(scan_idx):
    img_path = os.path.join(BASE_DIR, f"{scan_idx:04d}", "full.png")
    if not os.path.exists(img_path):
        return None

    img = np.array(Image.open(img_path))
    h, w = img.shape[:2]
    page = scan_idx // 40

    # Detect column gaps
    col_gaps = detect_column_gaps(img)
    if col_gaps is None or len(col_gaps) < 5:
        return None

    # Column centers = midpoints between gaps
    # But we also need the leftmost and rightmost card centers
    # Cards are between gaps. First card is left of first gap.
    # Card width ≈ gap_spacing - gap_width

    # Estimate card centers from gaps
    # If gaps are at positions g0, g1, g2, ..., g6 (7 internal gaps for 8 columns)
    # Then card centers are roughly: (g0 - half_spacing, (g0+g1)/2, (g1+g2)/2, ...)
    if len(col_gaps) >= 7:
        spacing = np.mean(np.diff(col_gaps))
        col_centers = []
        col_centers.append(col_gaps[0] - spacing / 2)  # col 0
        for i in range(len(col_gaps) - 1):
            col_centers.append((col_gaps[i] + col_gaps[i + 1]) / 2)
        col_centers.append(col_gaps[-1] + spacing / 2)  # last col
    else:
        return None

    # Detect row edges
    top_edges, bot_edges, profile = detect_row_edges(img, col_centers)

    # Compute row centers from top/bottom edges
    row_centers = []
    if len(top_edges) >= 5 and len(bot_edges) >= 5:
        for i in range(min(len(top_edges), len(bot_edges))):
            row_centers.append((top_edges[i] + bot_edges[i]) / 2)
    elif len(top_edges) >= 5:
        # Use top edges + estimated card height
        for t in top_edges[:5]:
            row_centers.append(t + 155)  # rough card half-height

    return {
        'scan_idx': scan_idx,
        'page': page,
        'col_gaps': col_gaps,
        'col_centers': [float(c) for c in col_centers],
        'top_edges': top_edges,
        'bot_edges': bot_edges,
        'row_centers': [float(c) for c in row_centers],
        'n_col_gaps': len(col_gaps),
        'n_top_edges': len(top_edges),
    }


def main():
    # Sample evenly, ~3 per page-mod-3 group
    samples = []
    for i in range(0, 2342, 40):  # one per page
        samples.append(i)

    print(f"Processing {len(samples)} images (one per page)...")

    all_results = []
    with Pool(min(cpu_count(), 8)) as pool:
        for r in pool.imap_unordered(process_image, samples, chunksize=5):
            if r is not None:
                all_results.append(r)

    print(f"Got results from {len(all_results)} images")

    # Analyze column positions
    all_col_centers = []
    all_gx = []
    all_ox = []
    for r in all_results:
        if len(r['col_centers']) >= 8:
            all_col_centers.append(r['col_centers'][:8])
            all_gx.append(r['col_centers'][0])
            spacings = np.diff(r['col_centers'][:8])
            all_ox.append(np.mean(spacings))

    if all_gx:
        gx_arr = np.array(all_gx)
        ox_arr = np.array(all_ox)
        print(f"\n=== COLUMN ANALYSIS (4K) ===")
        print(f"  GX (col 0 center): mean={gx_arr.mean():.1f}  std={gx_arr.std():.2f}  "
              f"range=[{gx_arr.min():.0f}, {gx_arr.max():.0f}]")
        print(f"  OX (col spacing):  mean={ox_arr.mean():.1f}  std={ox_arr.std():.2f}  "
              f"range=[{ox_arr.min():.0f}, {ox_arr.max():.0f}]")
        print(f"  At 1080p: GX={gx_arr.mean()/2:.1f}  OX={ox_arr.mean()/2:.1f}")

    # Analyze row positions per page
    print(f"\n=== ROW ANALYSIS (4K) ===")

    pages_with_rows = [(r['page'], r['row_centers'], r['top_edges'], r['bot_edges'])
                       for r in all_results if len(r['row_centers']) >= 5]

    if pages_with_rows:
        all_gy = []
        all_oy = []
        all_card_h = []
        page_gy = {}

        for page, row_c, top_e, bot_e in pages_with_rows:
            all_gy.append(row_c[0])
            spacings = np.diff(row_c[:5])
            all_oy.append(np.mean(spacings))
            page_gy[page] = row_c[0]

            if len(top_e) >= 5 and len(bot_e) >= 5:
                for i in range(min(len(top_e), len(bot_e))):
                    all_card_h.append(bot_e[i] - top_e[i])

        gy_arr = np.array(all_gy)
        oy_arr = np.array(all_oy)

        print(f"  Pages with 5 detected rows: {len(pages_with_rows)}")
        print(f"  GY (row 0 center): mean={gy_arr.mean():.1f}  std={gy_arr.std():.2f}  "
              f"range=[{gy_arr.min():.0f}, {gy_arr.max():.0f}]")
        print(f"  OY (row spacing):  mean={oy_arr.mean():.1f}  std={oy_arr.std():.2f}  "
              f"range=[{oy_arr.min():.0f}, {oy_arr.max():.0f}]")
        print(f"  At 1080p: GY={gy_arr.mean()/2:.1f}  OY={oy_arr.mean()/2:.1f}")

        if all_card_h:
            ch_arr = np.array(all_card_h)
            print(f"  Card height: mean={ch_arr.mean():.1f}  std={ch_arr.std():.2f}  "
                  f"range=[{ch_arr.min():.0f}, {ch_arr.max():.0f}]")
            print(f"  At 1080p: {ch_arr.mean()/2:.1f}")

        # Compare GY with expected (using fit OY + page offsets)
        print(f"\n  Per-page GY vs fitted prediction:")
        errors = []
        for page, gy in sorted(page_gy.items()):
            scroll = page_offsets.get(page, 0)
            # From fit: lock centroid row0 = 387.5 + scroll
            # GY_card_center ≈ lock_centroid + offset
            # We don't know the offset exactly, so just check consistency
            errors.append(gy)

        if errors:
            err_arr = np.array(errors)
            print(f"    GY mean={err_arr.mean():.1f}  std={err_arr.std():.2f}")

            # Check if per-page offsets from fit match the GY variation
            fitted_pages = sorted(page_gy.keys())
            if len(fitted_pages) > 5:
                base_gy = np.mean([page_gy[p] for p in fitted_pages
                                   if page_offsets.get(p, 0) > -3])  # mod3==0 pages
                print(f"    Base GY (mod3=0 pages): {base_gy:.1f}")

                for mod3 in [0, 1, 2]:
                    mod_pages = [p for p in fitted_pages if p % 3 == mod3]
                    if mod_pages:
                        mod_gy = np.mean([page_gy[p] for p in mod_pages])
                        mod_scroll = np.mean([page_offsets.get(p, 0) for p in mod_pages])
                        print(f"    mod3={mod3}: mean_GY={mod_gy:.1f}  "
                              f"mean_fit_scroll={mod_scroll:.1f}  "
                              f"predicted_GY={base_gy + mod_scroll:.1f}  "
                              f"error={mod_gy - (base_gy + mod_scroll):.1f}")

    # Also check column width
    all_card_w = []
    for r in all_results:
        if len(r['col_gaps']) >= 7:
            gaps = r['col_gaps']
            spacing = np.mean(np.diff(gaps))
            # Estimate card width from gap positions
            # Gap is narrow, card fills most of the spacing
            # Card width ≈ spacing - gap_width (gap ≈ 40-50px at 4K)
            all_card_w.append(spacing)

    if all_card_w:
        cw_arr = np.array(all_card_w)
        print(f"\n  Col spacing (≈card_w + gap): mean={cw_arr.mean():.1f}  "
              f"at 1080p: {cw_arr.mean()/2:.1f}")

    # === FINAL RECOMMENDED PARAMETERS ===
    print(f"\n{'='*60}")
    print(f"  RECOMMENDED GRID PARAMETERS (at 1080p)")
    print(f"{'='*60}")
    if all_gx and all_gy:
        rec_gx = gx_arr.mean() / 2
        rec_gy = gy_arr.mean() / 2
        rec_ox = ox_arr.mean() / 2
        rec_oy = oy_arr.mean() / 2
        print(f"  GX = {rec_gx:.1f}  (current: 180.0)")
        print(f"  GY = {rec_gy:.1f}  (current: 253.0)")
        print(f"  OX = {rec_ox:.1f}  (current: 145.0)")
        print(f"  OY = {rec_oy:.1f}  (current: 166.0)")
        print(f"  + per-page scroll offset (mod3 pattern: 0, -5.5, -11.5 at 1080p)")

    # Count how many images had good row detection
    row_counts = [r['n_top_edges'] for r in all_results]
    print(f"\n  Row detection reliability:")
    for n in range(8):
        cnt = sum(1 for c in row_counts if c == n)
        if cnt > 0:
            print(f"    {n} rows detected: {cnt}/{len(all_results)} images")


if __name__ == "__main__":
    main()
