"""
Visualize grid icon slot detection for both artifact and weapon tabs.

Draws cell bounding boxes and icon slot boxes ONLY where an icon is detected
by mean-color classification (same logic as grid_icon_detector.rs).

Icon slot interpretation:
  Artifact: S1=lock, S2=astral(if locked)/elixir, S3=elixir(if lock+astral)
  Weapon:   S1=refinement(always present), S2=lock(optional)

Uses per-image calibration: finds pink lock centroids in known-locked cells
(from GT/scan export), computes median offset.

Usage:
    python scripts/visualize_grid_icons.py
"""
import json, os, numpy as np
from PIL import Image, ImageDraw

# ================================================================
# Paths
# ================================================================
ART_DIR  = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
WPN_DIR  = "F:/Codes/genshin/yas/target/release/debug_images/weapons"
OUT_DIR  = "F:/Codes/genshin/yas/scripts/grid_viz"

ART_GT_FILE = "F:/Codes/genshin/yas/scripts/grid_ground_truth.json"
_export_dir = "F:/Codes/genshin/yas/target/release"
_exports = sorted([f for f in os.listdir(_export_dir)
                   if f.startswith("good_export_") and f.endswith(".json")])
SCAN_FILE = os.path.join(_export_dir, _exports[-1]) if _exports else None

# ================================================================
# Fitted grid params (4K resolution)
# ================================================================
GX_LOCK  = 262.3
GY_LOCK  = 386.8
OX       = 292.8
OY       = 350.4
LOCK_DX  = -97.3
LOCK_DY  = -118.5
GX       = GX_LOCK - LOCK_DX   # ~359.6
GY       = GY_LOCK - LOCK_DY   # ~505.3

SLOT_SPACING = 45.3   # vertical spacing between icon slots (4K)
CROP_HALF    = 8       # half-size of sampling area (4K pixels)
BOX_SIZE     = 10      # half-size of drawn slot box (4K pixels)
CARD_W       = 247
CARD_H       = 309

COLS = 8
ROWS = 5
ITEMS_PER_PAGE = 40

# ================================================================
# Color classification thresholds (matching grid_icon_detector.rs)
# ================================================================
LOCK_R_MIN       = 180.0
LOCK_RG_DIFF_MIN = 50.0
ASTRAL_GB_DIFF_MIN = 100.0
ELIXIR_BG_DIFF_MIN = 20.0
ELIXIR_B_MIN     = 180.0


def is_lock_color(r, g, b):
    return r > LOCK_R_MIN and (r - g) > LOCK_RG_DIFF_MIN

def is_astral_color(r, g, b):
    return (g - b) > ASTRAL_GB_DIFF_MIN

def is_elixir_color(r, g, b):
    return (b - g) > ELIXIR_BG_DIFF_MIN and b > ELIXIR_B_MIN


def sample_mean_color(img_np, x, y):
    """Mean RGB of a small crop area around (x, y) in 4K pixel coords."""
    h, w = img_np.shape[:2]
    x1 = max(0, int(x - CROP_HALF))
    y1 = max(0, int(y - CROP_HALF))
    x2 = min(w, int(x + CROP_HALF))
    y2 = min(h, int(y + CROP_HALF))
    if x1 >= x2 or y1 >= y2:
        return (0.0, 0.0, 0.0)
    crop = img_np[y1:y2, x1:x2]
    return (crop[:,:,0].mean(), crop[:,:,1].mean(), crop[:,:,2].mean())


def find_pink_centroids(img_np, page_start, page_items, skip_idx, lock_lookup):
    """Find pink lock centroids in known-locked cells for calibration."""
    h, w = img_np.shape[:2]
    res_x_list = []
    res_y_list = []
    for i in range(page_items):
        idx = page_start + i
        if idx == skip_idx:
            continue
        if not lock_lookup.get(idx, False):
            continue
        row = i // COLS
        col = i % COLS
        ex = GX_LOCK + col * OX
        ey = GY_LOCK + row * OY
        lx1 = max(0, int(ex) - 40)
        ly1 = max(0, int(ey) - 40)
        lx2 = min(w, int(ex) + 40)
        ly2 = min(h, int(ey) + 40)
        patch = img_np[ly1:ly2, lx1:lx2, :3]
        r = patch[:,:,0].astype(np.int16)
        g = patch[:,:,1].astype(np.int16)
        b = patch[:,:,2].astype(np.int16)
        mask = (r > 180) & ((r - g) > 60) & ((r - b) > 50) & (b > 70)
        if np.sum(mask) < 10:
            continue
        ys, xs = np.where(mask)
        cx = float(np.mean(xs)) + lx1
        cy = float(np.mean(ys)) + ly1
        res_x_list.append(cx - ex)
        res_y_list.append(cy - ey)
    if res_x_list:
        return float(np.median(res_x_list)), float(np.median(res_y_list))
    return 0.0, 0.0


def draw_overlay(img_path, scan_idx, total_items, lock_lookup, mode):
    """
    Draw grid + detected icon boxes on a full screenshot.
    Only draws an icon box when the color classifier fires.
    """
    if not os.path.exists(img_path):
        return None

    img = Image.open(img_path).convert("RGB")
    img_np = np.array(img)
    draw = ImageDraw.Draw(img)
    h, w = img_np.shape[:2]

    page = scan_idx // ITEMS_PER_PAGE
    page_start = page * ITEMS_PER_PAGE
    page_items = min(ITEMS_PER_PAGE, total_items - page_start)

    off_x, off_y = find_pink_centroids(img_np, page_start, page_items,
                                       scan_idx, lock_lookup)

    for i in range(page_items):
        row = i // COLS
        col = i % COLS
        idx = page_start + i

        # Calibrated cell center
        cx = GX + col * OX + off_x
        cy = GY + row * OY + off_y

        # Cell bounding box
        x1 = int(cx - CARD_W // 2)
        y1 = int(cy - CARD_H // 2)
        x2 = int(cx + CARD_W // 2)
        y2 = int(cy + CARD_H // 2)
        if idx == scan_idx:
            draw.rectangle([x1, y1, x2, y2], outline=(128, 128, 128), width=1)
        else:
            draw.rectangle([x1, y1, x2, y2], outline=(60, 255, 60), width=2)
        draw.text((x1 + 4, y1 + 4), f"{idx}", fill=(255, 255, 255))

        # Icon slot positions
        sx = cx + LOCK_DX
        s1y = cy + LOCK_DY
        s2y = s1y + SLOT_SPACING
        s3y = s2y + SLOT_SPACING

        # Sample colors at each slot
        c1 = sample_mean_color(img_np, sx, s1y)
        c2 = sample_mean_color(img_np, sx, s2y)
        c3 = sample_mean_color(img_np, sx, s3y)

        if mode == "artifact":
            # S1: lock?
            if is_lock_color(*c1):
                _draw_box(draw, sx, s1y, (255, 80, 200), "L")
                # S2: astral? or elixir?
                if is_astral_color(*c2):
                    _draw_box(draw, sx, s2y, (255, 255, 0), "A")
                    # S3: elixir?
                    if is_elixir_color(*c3):
                        _draw_box(draw, sx, s3y, (80, 120, 255), "E")
                elif is_elixir_color(*c2):
                    _draw_box(draw, sx, s2y, (80, 120, 255), "E")
            else:
                # No lock — check if elixir shifted up to S1
                if is_elixir_color(*c1):
                    _draw_box(draw, sx, s1y, (80, 120, 255), "E")

        elif mode == "weapon":
            # S1: refinement badge (always present — not pink, not astral, not elixir)
            # We don't detect refinement by color; just check S2 for lock.
            if is_lock_color(*c2):
                _draw_box(draw, sx, s2y, (255, 80, 200), "L")

    draw.text((20, 20),
              f"{mode.upper()} page {page}  offset=({off_x:+.1f}, {off_y:+.1f})",
              fill=(255, 255, 255))
    return img


def _draw_box(draw, cx, cy, color, label):
    bx1 = int(cx - BOX_SIZE)
    by1 = int(cy - BOX_SIZE)
    bx2 = int(cx + BOX_SIZE)
    by2 = int(cy + BOX_SIZE)
    draw.rectangle([bx1, by1, bx2, by2], outline=color, width=2)
    draw.text((bx2 + 3, by1), label, fill=color)


def main():
    os.makedirs(OUT_DIR, exist_ok=True)

    # Load artifact GT
    art_lock = {}
    if os.path.exists(ART_GT_FILE):
        with open(ART_GT_FILE) as f:
            for g in json.load(f)['items']:
                art_lock[g['idx']] = g['lock']

    # Load weapon lock info from scan export
    wpn_lock = {}
    total_weapons = 0
    if SCAN_FILE:
        with open(SCAN_FILE, encoding='utf-8') as f:
            data = json.load(f)
        for i, w in enumerate(data.get('weapons', [])):
            wpn_lock[i] = w.get('lock', False)
        total_weapons = len(data.get('weapons', []))
        print(f"Scan export: {os.path.basename(SCAN_FILE)}")
        print(f"  Weapons: {total_weapons}, locked: {sum(wpn_lock.values())}")

    total_artifacts = max(art_lock.keys()) + 1 if art_lock else 0
    print(f"  Artifacts: {total_artifacts}, locked: {sum(art_lock.values())}")

    # --- Artifact samples ---
    art_samples = [0, 400, 710, 1408, 2108]
    art_samples = [s for s in art_samples if s < total_artifacts]

    for idx in art_samples:
        src = os.path.join(ART_DIR, f"{idx:04d}", "full.png")
        result = draw_overlay(src, idx, total_artifacts, art_lock, "artifact")
        if result:
            out = os.path.join(OUT_DIR, f"art_{idx:04d}.png")
            result.save(out)
            print(f"  artifact {idx:4d} -> {out}")

    # --- Weapon samples ---
    wpn_samples = [0, 45, 100, 300, 500]
    wpn_samples = [s for s in wpn_samples if s < total_weapons]

    for idx in wpn_samples:
        src = os.path.join(WPN_DIR, f"{idx:04d}", "full.png")
        result = draw_overlay(src, idx, total_weapons, wpn_lock, "weapon")
        if result:
            out = os.path.join(OUT_DIR, f"wpn_{idx:04d}.png")
            result.save(out)
            print(f"  weapon   {idx:4d} -> {out}")

    print(f"\nAll saved to {OUT_DIR}/")


if __name__ == "__main__":
    main()
