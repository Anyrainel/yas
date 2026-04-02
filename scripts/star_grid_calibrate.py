"""
Grid calibration using FFT-based 2D cross-correlation.

Known: grid shape (5 rows, 8 cols), OX=292.8, OY=350.4, CARD_W=247, CARD_H=306.
Unknown: gx, gy (cell 0,0 center).

Approach:
1. Compute lightness signal of the full screenshot
2. Build a 2D template: +1 at bright areas (card edges near gaps), -1 at gap areas
3. Cross-correlate via FFT → correlation map
4. Peak of correlation map → best (gx, gy)

Validated against pink-centroid ground truth for artifacts.
"""
import json, os
import numpy as np
from PIL import Image, ImageDraw
from scipy.signal import fftconvolve

ART_DIR  = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
WPN_DIR  = "F:/Codes/genshin/yas/target/release/debug_images/weapons"
OUT_DIR  = "F:/Codes/genshin/yas/scripts/grid_viz"

ART_GT_FILE = "F:/Codes/genshin/yas/scripts/grid_ground_truth.json"
_export_dir = "F:/Codes/genshin/yas/target/release"
_exports = sorted([f for f in os.listdir(_export_dir)
                   if f.startswith("good_export_") and f.endswith(".json")])
SCAN_FILE = os.path.join(_export_dir, _exports[-1]) if _exports else None

# ================================================================
# Known grid constants (4K)
# ================================================================
GX_LOCK  = 262.3
GY_LOCK  = 386.8
OX       = 292.8
OY       = 350.4
LOCK_DX  = -97.3
LOCK_DY  = -118.5
GX       = GX_LOCK - LOCK_DX   # 359.6
GY       = GY_LOCK - LOCK_DY   # 505.3

SLOT_SPACING = 45.3
CROP_HALF    = 8
BOX_SIZE     = 10
CARD_W       = 247
CARD_H       = 306
COLS = 8
ROWS = 5
ITEMS_PER_PAGE = 40
AREA_BOTTOM = 1950

# ================================================================
# Pink centroid calibration (proven, artifact only)
# ================================================================
def find_pink_centroids(img_np, page_start, page_items, skip_idx, lock_lookup):
    h, w = img_np.shape[:2]
    res_x, res_y = [], []
    for i in range(page_items):
        idx = page_start + i
        if idx == skip_idx or not lock_lookup.get(idx, False):
            continue
        row, col = i // COLS, i % COLS
        ex = GX_LOCK + col * OX
        ey = GY_LOCK + row * OY
        lx1, ly1 = max(0, int(ex)-40), max(0, int(ey)-40)
        lx2, ly2 = min(w, int(ex)+40), min(h, int(ey)+40)
        patch = img_np[ly1:ly2, lx1:lx2, :3]
        r = patch[:,:,0].astype(np.int16)
        g = patch[:,:,1].astype(np.int16)
        b = patch[:,:,2].astype(np.int16)
        mask = (r > 180) & ((r-g) > 60) & ((r-b) > 50) & (b > 70)
        if np.sum(mask) < 10:
            continue
        ys, xs = np.where(mask)
        res_x.append(float(np.mean(xs)) + lx1 - ex)
        res_y.append(float(np.mean(ys)) + ly1 - ey)
    if res_x:
        return float(np.median(res_x)), float(np.median(res_y))
    return 0.0, 0.0


def get_gt_gxy(img_np, scan_idx, total_items, lock_lookup):
    page_start = (scan_idx // ITEMS_PER_PAGE) * ITEMS_PER_PAGE
    page_items = min(ITEMS_PER_PAGE, total_items - page_start)
    off_x, off_y = find_pink_centroids(img_np, page_start, page_items,
                                        scan_idx, lock_lookup)
    return GX + off_x, GY + off_y


# ================================================================
# FFT-based 2D cross-correlation calibration
# ================================================================
def compute_lightness(region):
    """Compute lightness (0-1) for each pixel."""
    r = region[:,:,0].astype(np.float64) / 255.0
    g = region[:,:,1].astype(np.float64) / 255.0
    b = region[:,:,2].astype(np.float64) / 255.0
    cmax = np.maximum(np.maximum(r, g), b)
    cmin = np.minimum(np.minimum(r, g), b)
    return (cmax + cmin) / 2.0


def build_grid_template(h, w):
    """
    Build a 2D template targeting bright→dark→bright transitions at grid
    boundaries for sharp X and Y signal.

    X signal: column gaps (-1) flanked by card content (bright).
    Y signal: row boundary edge detector — bright band at card bottom (white
    level band) then dark row gap then bright band at next card top (rarity bg).

    The key Y innovation: instead of marking entire card interiors or just gaps,
    we mark narrow bands at the card-gap transitions. This produces a sharp
    correlation peak because a small Y shift rapidly moves the bright/dark
    boundary in/out of alignment.
    """
    margin = 50
    t_w = int((COLS - 1) * OX + CARD_W + 2 * margin)
    t_h = int((ROWS - 1) * OY + CARD_H + 2 * margin)
    template = np.zeros((t_h, t_w), dtype=np.float64)

    origin_x = margin + CARD_W / 2
    origin_y = margin + CARD_H / 2

    gap_w = OX - CARD_W   # ~45.8px between columns
    gap_h = OY - CARD_H   # ~44.4px between rows

    # Width of bright bands flanking each row gap.
    # Narrower = sharper Y peak (less tolerance for shifts).
    # Must stay within consistently bright areas (white band at card bottom,
    # rarity background at card top).
    EDGE_W = 20

    # --- Column gaps (-1): sharp X signal ---
    for col in range(COLS - 1):
        gap_cx = origin_x + col * OX + OX / 2
        for row in range(ROWS):
            cy = origin_y + row * OY
            x1 = int(gap_cx - gap_w / 2)
            y1 = int(cy - CARD_H / 2)
            x2 = int(gap_cx + gap_w / 2)
            y2 = int(cy + CARD_H / 2)
            x1, y1 = max(0, x1), max(0, y1)
            x2, y2 = min(t_w, x2), min(t_h, y2)
            template[y1:y2, x1:x2] = -1.0

    # --- Row boundary edge detectors: bright (white band) → dark (gap) ---
    # Only use card BOTTOM (white band) as the +1 region — it's reliably
    # bright across all rarities for both artifacts and weapons.
    # Card top (rarity bg) is too variable for weapons (muted images).
    for row in range(ROWS - 1):
        # Bottom of card in row `row`
        y_bot = origin_y + row * OY + CARD_H / 2

        for col in range(COLS):
            cx = origin_x + col * OX
            x1 = max(0, int(cx - CARD_W / 2))
            x2 = min(t_w, int(cx + CARD_W / 2))

            # +1: bright band at card bottom (white level band area)
            by1 = max(0, int(y_bot - EDGE_W))
            by2 = int(y_bot)
            template[by1:by2, x1:x2] = 1.0

            # -1: dark row gap
            gy1 = max(0, int(y_bot))
            gy2 = min(t_h, int(y_bot + gap_h))
            template[gy1:gy2, x1:x2] = -1.0

    return template, origin_x, origin_y


def calibrate_fft(img_np, mode):
    """
    Find best (gx, gy) by FFT cross-correlation of lightness with grid template.
    """
    h, w = img_np.shape[:2]

    # Use lightness as signal — gaps are dark, cards are bright
    signal = compute_lightness(img_np)

    # Build template
    template, t_ox, t_oy = build_grid_template(h, w)

    # Cross-correlate using FFT
    corr = fftconvolve(signal, template[::-1, ::-1], mode='full')

    t_h, t_w = template.shape

    # Expected cell (0,0) center position
    if mode == "artifact":
        exp_gx, exp_gy = GX, GY
    else:
        exp_gx, exp_gy = GX, GY - 114

    # In correlation coordinates, the expected peak is at:
    exp_px = int(exp_gx + t_w - 1 - t_ox)
    exp_py = int(exp_gy + t_h - 1 - t_oy)

    # Search within ±60px of expected
    search_r = 60
    py_min = max(0, exp_py - search_r)
    py_max = min(corr.shape[0], exp_py + search_r)
    px_min = max(0, exp_px - search_r)
    px_max = min(corr.shape[1], exp_px + search_r)

    sub_corr = corr[py_min:py_max, px_min:px_max]
    peak_idx = np.unravel_index(np.argmax(sub_corr), sub_corr.shape)
    peak_py = peak_idx[0] + py_min
    peak_px = peak_idx[1] + px_min

    # Convert back to image coordinates
    gx = peak_px - (t_w - 1) + t_ox
    gy = peak_py - (t_h - 1) + t_oy

    return float(gx), float(gy), float(corr[peak_py, peak_px])


# ================================================================
# Drawing
# ================================================================
def is_lock_color(r, g, b):
    return r > 180.0 and (r - g) > 50.0
def is_astral_color(r, g, b):
    return (g - b) > 100.0
def is_elixir_color(r, g, b):
    return (b - g) > 20.0 and b > 180.0

def sample_mean_color(img_np, x, y):
    h, w = img_np.shape[:2]
    x1, y1 = max(0, int(x - CROP_HALF)), max(0, int(y - CROP_HALF))
    x2, y2 = min(w, int(x + CROP_HALF)), min(h, int(y + CROP_HALF))
    if x1 >= x2 or y1 >= y2:
        return (0.0, 0.0, 0.0)
    crop = img_np[y1:y2, x1:x2]
    return (crop[:,:,0].mean(), crop[:,:,1].mean(), crop[:,:,2].mean())

def _draw_box(draw, cx, cy, color, label):
    bx1, by1 = int(cx - BOX_SIZE), int(cy - BOX_SIZE)
    bx2, by2 = int(cx + BOX_SIZE), int(cy + BOX_SIZE)
    draw.rectangle([bx1, by1, bx2, by2], outline=color, width=2)
    draw.text((bx2 + 3, by1), label, fill=color)

def draw_overlay(img_path, gx, gy, page_items, mode, label):
    img = Image.open(img_path).convert("RGB")
    img_np = np.array(img)
    draw = ImageDraw.Draw(img)

    for i in range(page_items):
        row, col = i // COLS, i % COLS
        cx = gx + col * OX
        cy = gy + row * OY
        x1, y1 = int(cx - CARD_W//2), int(cy - CARD_H//2)
        x2, y2 = int(cx + CARD_W//2), int(cy + CARD_H//2)
        draw.rectangle([x1, y1, x2, y2], outline=(60, 255, 60), width=2)
        draw.text((x1+4, y1+4), f"{i}", fill=(255,255,255))

        sx = cx + LOCK_DX
        s1y = cy + LOCK_DY
        s2y = s1y + SLOT_SPACING
        s3y = s2y + SLOT_SPACING
        c1 = sample_mean_color(img_np, sx, s1y)
        c2 = sample_mean_color(img_np, sx, s2y)
        c3 = sample_mean_color(img_np, sx, s3y)

        if mode == "artifact":
            if is_lock_color(*c1):
                _draw_box(draw, sx, s1y, (255, 80, 200), "L")
                if is_astral_color(*c2):
                    _draw_box(draw, sx, s2y, (255, 255, 0), "A")
                    if is_elixir_color(*c3):
                        _draw_box(draw, sx, s3y, (80, 120, 255), "E")
                elif is_elixir_color(*c2):
                    _draw_box(draw, sx, s2y, (80, 120, 255), "E")
            else:
                if is_elixir_color(*c1):
                    _draw_box(draw, sx, s1y, (80, 120, 255), "E")
        elif mode == "weapon":
            if is_lock_color(*c2):
                _draw_box(draw, sx, s2y, (255, 80, 200), "L")

    draw.text((20, 20), label, fill=(255, 255, 255))
    return img


def main():
    os.makedirs(OUT_DIR, exist_ok=True)

    art_lock = {}
    if os.path.exists(ART_GT_FILE):
        with open(ART_GT_FILE) as f:
            for g in json.load(f)['items']:
                art_lock[g['idx']] = g['lock']
    total_art = max(art_lock.keys()) + 1 if art_lock else 0

    # ============================================================
    # Artifact: compare GT vs FFT method
    # ============================================================
    art_samples = [s for s in [0, 400, 710, 1408, 2108, 2300] if s < total_art]

    print("=== ARTIFACT: GT (pink) vs FFT correlation ===")
    print(f"{'idx':>6s}  {'GT gx':>7s} {'GT gy':>7s}  {'FFT gx':>7s} {'FFT gy':>7s}  "
          f"{'dx':>6s} {'dy':>6s} {'dist':>6s}")

    for idx in art_samples:
        src = os.path.join(ART_DIR, f"{idx:04d}", "full.png")
        if not os.path.exists(src):
            continue
        img_np = np.array(Image.open(src))

        gt_gx, gt_gy = get_gt_gxy(img_np, idx, total_art, art_lock)
        fft_gx, fft_gy, score = calibrate_fft(img_np, "artifact")

        dx, dy = fft_gx - gt_gx, fft_gy - gt_gy
        dist = np.sqrt(dx**2 + dy**2)
        print(f"{idx:6d}  {gt_gx:7.1f} {gt_gy:7.1f}  {fft_gx:7.1f} {fft_gy:7.1f}  "
              f"{dx:+6.1f} {dy:+6.1f} {dist:6.1f}")

        page_start = (idx // ITEMS_PER_PAGE) * ITEMS_PER_PAGE
        page_items = min(ITEMS_PER_PAGE, total_art - page_start)

        gt_img = draw_overlay(src, gt_gx, gt_gy, page_items, "artifact",
                              f"ART GT idx={idx} gx={gt_gx:.1f} gy={gt_gy:.1f}")
        gt_img.save(os.path.join(OUT_DIR, f"gt_art_{idx:04d}.png"))

        fft_img = draw_overlay(src, fft_gx, fft_gy, page_items, "artifact",
                               f"ART FFT idx={idx} gx={fft_gx:.1f} gy={fft_gy:.1f} "
                               f"err=({dx:+.1f},{dy:+.1f})")
        fft_img.save(os.path.join(OUT_DIR, f"fft_art_{idx:04d}.png"))

    # ============================================================
    # Weapon: FFT method
    # ============================================================
    print("\n=== WEAPON: FFT correlation ===")
    for idx in [0, 45, 100, 200, 300, 350, 400, 450, 500, 530]:
        src = os.path.join(WPN_DIR, f"{idx:04d}", "full.png")
        if not os.path.exists(src):
            continue
        img_np = np.array(Image.open(src))
        fft_gx, fft_gy, score = calibrate_fft(img_np, "weapon")
        print(f"  wpn {idx:4d}: gx={fft_gx:.1f} gy={fft_gy:.1f} score={score:.0f}")

        fft_img = draw_overlay(src, fft_gx, fft_gy, ITEMS_PER_PAGE, "weapon",
                               f"WPN FFT idx={idx} gx={fft_gx:.1f} gy={fft_gy:.1f}")
        fft_img.save(os.path.join(OUT_DIR, f"fft_wpn_{idx:04d}.png"))

    print(f"\nAll saved to {OUT_DIR}/")


if __name__ == "__main__":
    main()
