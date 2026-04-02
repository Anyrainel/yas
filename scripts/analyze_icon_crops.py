"""
Analyze cropped icon images to find color characteristics with wide margins.

Icons stack vertically in the top-left of each artifact card:
  Slot 1 (top): lock icon (if locked)
  Slot 2: astral mark (if present, requires lock)
  Slot 3: elixir mark (if present)
  Icons push up — if earlier slots are empty, later icons move up.

Backgrounds: 5-star gold, 4-star purple, elixir dark purple overlay.

We analyze each crop pixel-by-pixel to find distinguishing color properties.
"""

import numpy as np
from PIL import Image
import os

BASE = "F:/Codes/genshin/yas/target/release/debug_images"

crops = {
    "lock5":    os.path.join(BASE, "lock5.PNG"),
    "lock4":    os.path.join(BASE, "lock4.PNG"),
    "rarity5":  os.path.join(BASE, "rarity5.PNG"),
    "rarity4":  os.path.join(BASE, "rarity4.PNG"),
    "astral5":  os.path.join(BASE, "astral5.PNG"),
    "astral4":  os.path.join(BASE, "astral4.PNG"),
    "elixir":   os.path.join(BASE, "elixir.PNG"),
}

def analyze_crop(name, path):
    img = np.array(Image.open(path).convert("RGB"))
    h, w = img.shape[:2]
    r = img[:,:,0].astype(np.int16)
    g = img[:,:,1].astype(np.int16)
    b = img[:,:,2].astype(np.int16)

    print(f"\n{'='*70}")
    print(f"  {name}  ({w}x{h} pixels, {w*h} total)")
    print(f"{'='*70}")

    # Basic RGB stats
    print(f"\n  RGB ranges:")
    print(f"    R: [{r.min():3d}, {r.max():3d}]  mean={r.mean():.1f}  std={r.std():.1f}")
    print(f"    G: [{g.min():3d}, {g.max():3d}]  mean={g.mean():.1f}  std={g.std():.1f}")
    print(f"    B: [{b.min():3d}, {b.max():3d}]  mean={b.mean():.1f}  std={b.std():.1f}")

    # Derived channels
    rg = r - g
    rb = r - b
    gb = g - b
    brightness = (r + g + b) / 3.0
    max_rgb = np.maximum(np.maximum(r, g), b)
    min_rgb = np.minimum(np.minimum(r, g), b)
    saturation = (max_rgb - min_rgb).astype(np.float32)
    # Avoid div by zero
    sat_norm = np.where(max_rgb > 0, saturation / max_rgb.astype(np.float32), 0)

    print(f"\n  Derived channels:")
    print(f"    R-G:   [{rg.min():+4d}, {rg.max():+4d}]  mean={rg.mean():+6.1f}")
    print(f"    R-B:   [{rb.min():+4d}, {rb.max():+4d}]  mean={rb.mean():+6.1f}")
    print(f"    G-B:   [{gb.min():+4d}, {gb.max():+4d}]  mean={gb.mean():+6.1f}")
    print(f"    Bright:[{brightness.min():5.1f}, {brightness.max():5.1f}]  mean={brightness.mean():.1f}")
    print(f"    Sat:   [{saturation.min():.0f}, {saturation.max():.0f}]  mean={saturation.mean():.1f}")

    # Now analyze distinct regions within the crop
    # The crops have: dark square background + icon foreground
    # Let's segment by brightness and color

    # Dark square region (low brightness)
    dark_mask = brightness < 80
    mid_mask = (brightness >= 80) & (brightness < 150)
    bright_mask = brightness >= 150

    for label, mask in [("Dark (bright<80)", dark_mask),
                         ("Mid (80-150)", mid_mask),
                         ("Bright (>=150)", bright_mask)]:
        count = np.sum(mask)
        if count == 0:
            continue
        print(f"\n  {label}: {count} px ({count*100/(w*h):.1f}%)")
        print(f"    R: [{r[mask].min():3d}, {r[mask].max():3d}]  mean={r[mask].mean():.1f}")
        print(f"    G: [{g[mask].min():3d}, {g[mask].max():3d}]  mean={g[mask].mean():.1f}")
        print(f"    B: [{b[mask].min():3d}, {b[mask].max():3d}]  mean={b[mask].mean():.1f}")
        print(f"    R-G: [{rg[mask].min():+4d}, {rg[mask].max():+4d}]")
        print(f"    G-B: [{gb[mask].min():+4d}, {gb[mask].max():+4d}]")

    # === Icon-specific analysis ===
    # For lock: pink pixels
    pink_mask = (r > 180) & (rg > 60) & (rb > 50) & (b > 70)
    pink_count = np.sum(pink_mask)
    if pink_count > 0:
        print(f"\n  Pink pixels (lock icon body): {pink_count}")
        print(f"    R: [{r[pink_mask].min()}, {r[pink_mask].max()}]")
        print(f"    G: [{g[pink_mask].min()}, {g[pink_mask].max()}]")
        print(f"    B: [{b[pink_mask].min()}, {b[pink_mask].max()}]")

    # For astral: yellow pixels
    yellow_mask = (r > 220) & (g > 170) & (b < 80)
    yellow_count = np.sum(yellow_mask)
    if yellow_count > 0:
        print(f"\n  Yellow pixels (astral star body): {yellow_count}")
        print(f"    R: [{r[yellow_mask].min()}, {r[yellow_mask].max()}]")
        print(f"    G: [{g[yellow_mask].min()}, {g[yellow_mask].max()}]")
        print(f"    B: [{b[yellow_mask].min()}, {b[yellow_mask].max()}]")

    # For elixir: purple/blue pixels
    elixir_mask = (b > 150) & (b > r + 30) & (b > g + 30)
    elixir_count = np.sum(elixir_mask)
    if elixir_count > 0:
        print(f"\n  Blue-purple pixels (elixir icon): {elixir_count}")
        print(f"    R: [{r[elixir_mask].min()}, {r[elixir_mask].max()}]")
        print(f"    G: [{g[elixir_mask].min()}, {g[elixir_mask].max()}]")
        print(f"    B: [{b[elixir_mask].min()}, {b[elixir_mask].max()}]")

    # === KEY QUESTION: What makes the dark square unique? ===
    # The dark square is semi-transparent black overlaid on the rarity bg.
    # Let's look at the CORNERS of the crop — those should be rarity bg (no dark square)
    # and the CENTER should be dark square.
    corner_size = max(3, min(w, h) // 6)
    corners = np.zeros((h, w), dtype=bool)
    corners[:corner_size, :corner_size] = True
    corners[:corner_size, -corner_size:] = True
    corners[-corner_size:, :corner_size] = True
    corners[-corner_size:, -corner_size:] = True

    center = np.zeros((h, w), dtype=bool)
    cy, cx = h // 2, w // 2
    cs = max(3, min(w, h) // 4)
    center[cy-cs:cy+cs, cx-cs:cx+cs] = True

    print(f"\n  Corner pixels ({np.sum(corners)} px, should be bg/edge):")
    print(f"    R: [{r[corners].min():3d}, {r[corners].max():3d}]  mean={r[corners].mean():.1f}")
    print(f"    G: [{g[corners].min():3d}, {g[corners].max():3d}]  mean={g[corners].mean():.1f}")
    print(f"    B: [{b[corners].min():3d}, {b[corners].max():3d}]  mean={b[corners].mean():.1f}")
    print(f"    Brightness: mean={brightness[corners].mean():.1f}")

    print(f"\n  Center pixels ({np.sum(center)} px, should be dark sq / icon):")
    print(f"    R: [{r[center].min():3d}, {r[center].max():3d}]  mean={r[center].mean():.1f}")
    print(f"    G: [{g[center].min():3d}, {g[center].max():3d}]  mean={g[center].mean():.1f}")
    print(f"    B: [{b[center].min():3d}, {b[center].max():3d}]  mean={b[center].mean():.1f}")
    print(f"    Brightness: mean={brightness[center].mean():.1f}")

    # === Horizontal and vertical brightness profiles ===
    # Sample middle row and middle column
    mid_row = img[h//2, :, :].astype(np.float32)
    mid_col = img[:, w//2, :].astype(np.float32)
    print(f"\n  Middle row brightness profile (left→right, {w} px):")
    row_bright = mid_row.mean(axis=1)
    # Show 8 evenly spaced samples
    samples = np.linspace(0, w-1, min(10, w)).astype(int)
    vals = [f"{row_bright[s]:.0f}" for s in samples]
    print(f"    {' → '.join(vals)}")

    print(f"  Middle col brightness profile (top→bottom, {h} px):")
    col_bright = mid_col.mean(axis=1)
    samples = np.linspace(0, h-1, min(10, h)).astype(int)
    vals = [f"{col_bright[s]:.0f}" for s in samples]
    print(f"    {' → '.join(vals)}")


def compare_backgrounds():
    """Compare the 5-star and 4-star background colors at the icon slot positions."""
    print(f"\n\n{'#'*70}")
    print(f"  CROSS-CROP COMPARISON: Background colors")
    print(f"{'#'*70}")

    # Load all crops
    imgs = {}
    for name, path in crops.items():
        imgs[name] = np.array(Image.open(path).convert("RGB"))

    # Compare rarity backgrounds
    r5 = imgs['rarity5']
    r4 = imgs['rarity4']

    print(f"\n  Rarity 5 background (full crop avg):")
    print(f"    R={r5[:,:,0].mean():.1f}  G={r5[:,:,1].mean():.1f}  B={r5[:,:,2].mean():.1f}")
    print(f"  Rarity 4 background (full crop avg):")
    print(f"    R={r4[:,:,0].mean():.1f}  G={r4[:,:,1].mean():.1f}  B={r4[:,:,2].mean():.1f}")

    # The key distinguishing features between each icon type
    print(f"\n  === DISCRIMINATION ANALYSIS ===")
    print(f"  For each pair, what separates them?\n")

    pairs = [
        ("lock5", "rarity5", "Lock vs Empty (5-star)"),
        ("lock4", "rarity4", "Lock vs Empty (4-star)"),
        ("astral5", "rarity5", "Astral vs Empty (5-star)"),
        ("astral4", "rarity4", "Astral vs Empty (4-star)"),
        ("astral5", "lock5", "Astral vs Lock (5-star)"),
        ("elixir", "rarity5", "Elixir vs Empty (5-star)"),
        ("elixir", "lock5", "Elixir vs Lock (5-star)"),
    ]

    for name_a, name_b, desc in pairs:
        a = imgs[name_a].astype(np.float32)
        b = imgs[name_b].astype(np.float32)
        # Resize to same size if needed (use smaller)
        h = min(a.shape[0], b.shape[0])
        w = min(a.shape[1], b.shape[1])
        a = a[:h, :w]
        b = b[:h, :w]

        diff = a.mean(axis=(0,1)) - b.mean(axis=(0,1))
        a_bright = a.mean()
        b_bright = b.mean()

        print(f"  {desc}:")
        print(f"    Mean RGB diff (A-B): R={diff[0]:+.1f}  G={diff[1]:+.1f}  B={diff[2]:+.1f}")
        print(f"    Brightness: {name_a}={a_bright:.1f}  {name_b}={b_bright:.1f}  diff={a_bright-b_bright:+.1f}")

    # === Per-slot analysis ===
    # The crops are one icon slot. Let's check if we can identify icon type
    # by looking at specific sub-regions.
    print(f"\n  === PER-REGION IDENTIFICATION ===")
    print(f"  Sampling specific sub-regions of each crop\n")

    for name in sorted(imgs.keys()):
        img = imgs[name]
        h, w = img.shape[:2]
        r = img[:,:,0].astype(np.int16)
        g = img[:,:,1].astype(np.int16)
        b = img[:,:,2].astype(np.int16)

        # Top-left quadrant (dark square corner if icon present)
        q = max(2, min(h, w) // 4)
        tl = img[:q, :q]
        # Center
        ch, cw = h//2, w//2
        ctr = img[ch-q:ch+q, cw-q:cw+q]

        tl_bright = tl.mean()
        ctr_bright = ctr.mean()

        # Dominant color channel
        r_mean = r.mean()
        g_mean = g.mean()
        b_mean = b.mean()
        dom = "R" if r_mean >= g_mean and r_mean >= b_mean else ("G" if g_mean >= b_mean else "B")

        # Color ratios
        rg_ratio = r_mean / max(g_mean, 1)
        rb_ratio = r_mean / max(b_mean, 1)
        gb_ratio = g_mean / max(b_mean, 1)

        print(f"  {name:10s}: bright={tl_bright:5.1f}/{ctr_bright:5.1f} (TL/center)  "
              f"dom={dom}  R/G={rg_ratio:.2f}  R/B={rb_ratio:.2f}  G/B={gb_ratio:.2f}  "
              f"mean=({r_mean:.0f},{g_mean:.0f},{b_mean:.0f})")


def analyze_dark_square_signature():
    """
    The dark square is the key feature. Let's characterize what makes
    'has dark square' vs 'no dark square' distinguishable.
    """
    print(f"\n\n{'#'*70}")
    print(f"  DARK SQUARE SIGNATURE ANALYSIS")
    print(f"  What makes a slot with an icon different from empty?")
    print(f"{'#'*70}")

    imgs = {}
    for name, path in crops.items():
        imgs[name] = np.array(Image.open(path).convert("RGB"))

    # For each icon crop vs its matching empty background:
    # 1. The dark square reduces brightness uniformly
    # 2. The dark square reduces saturation (mixes with black)
    # 3. The dark square has rounded corners — edges transition

    icon_vs_bg = [
        ("lock5", "rarity5"),
        ("lock4", "rarity4"),
        ("astral5", "rarity5"),
        ("astral4", "rarity4"),
        ("elixir", "rarity5"),
    ]

    for icon_name, bg_name in icon_vs_bg:
        icon = imgs[icon_name].astype(np.float32)
        bg = imgs[bg_name].astype(np.float32)
        h = min(icon.shape[0], bg.shape[0])
        w = min(icon.shape[1], bg.shape[1])
        icon = icon[:h, :w]
        bg = bg[:h, :w]

        # Per-pixel brightness
        icon_bright = icon.mean(axis=2)
        bg_bright = bg.mean(axis=2)

        # Ratio: icon / bg (dark square ≈ 0.3-0.6 of bg)
        ratio = np.where(bg_bright > 10, icon_bright / bg_bright, 1.0)

        # The dark square should show as a uniform low ratio region
        # Let's look at the ratio in center vs edges
        q = max(2, min(h, w) // 4)

        center_ratio = ratio[h//2-q:h//2+q, w//2-q:w//2+q]
        edge_ratio_top = ratio[:q, :]
        edge_ratio_bot = ratio[-q:, :]

        # Also: what does the dark square do to channel ratios?
        # If bg is (R, G, B) and dark square applies alpha blend with black:
        # result = bg * (1-alpha) → all channels scale by same factor
        # So R/G and R/B ratios should be PRESERVED through the dark square
        icon_rg = np.where(icon[:,:,1] > 5, icon[:,:,0] / icon[:,:,1], 0)
        bg_rg = np.where(bg[:,:,1] > 5, bg[:,:,0] / bg[:,:,1], 0)
        icon_rb = np.where(icon[:,:,2] > 5, icon[:,:,0] / icon[:,:,2], 0)
        bg_rb = np.where(bg[:,:,2] > 5, bg[:,:,0] / bg[:,:,2], 0)

        print(f"\n  {icon_name} vs {bg_name}:")
        print(f"    Brightness ratio (icon/bg):")
        print(f"      Center: mean={center_ratio.mean():.3f}  std={center_ratio.std():.3f}")
        print(f"      Top edge: mean={edge_ratio_top.mean():.3f}")
        print(f"      Bot edge: mean={edge_ratio_bot.mean():.3f}")
        print(f"    R/G ratio preserved? icon_center={icon_rg[h//2-q:h//2+q, w//2-q:w//2+q].mean():.3f}  "
              f"bg={bg_rg[h//2-q:h//2+q, w//2-q:w//2+q].mean():.3f}")

        # Key metric: can we detect dark square by checking if brightness
        # is significantly below expected rarity bg?
        # 5-star bg: ~130-160 brightness, dark square: ~50-80
        # 4-star bg: ~90-110 brightness, dark square: ~50-70
        print(f"    Pixel brightness:")
        print(f"      Icon center: {icon_bright[h//2-q:h//2+q, w//2-q:w//2+q].mean():.1f}")
        print(f"      BG center:   {bg_bright[h//2-q:h//2+q, w//2-q:w//2+q].mean():.1f}")
        print(f"      Difference:  {(bg_bright - icon_bright)[h//2-q:h//2+q, w//2-q:w//2+q].mean():.1f}")


def analyze_icon_colors_detail():
    """
    Detailed color analysis per icon type to find wide-margin discriminators.
    """
    print(f"\n\n{'#'*70}")
    print(f"  ICON COLOR DISCRIMINATORS")
    print(f"  What color features have the WIDEST separation margins?")
    print(f"{'#'*70}")

    imgs = {}
    for name, path in crops.items():
        imgs[name] = np.array(Image.open(path).convert("RGB"))

    # For each crop, compute a feature vector of various color metrics
    # Then find which metrics best separate each pair

    features = {}
    for name, img in imgs.items():
        r = img[:,:,0].astype(np.float32)
        g = img[:,:,1].astype(np.float32)
        b = img[:,:,2].astype(np.float32)
        h, w = r.shape

        # Sample center region (avoid anti-aliased edges of dark square)
        q = max(2, min(h, w) // 4)
        cy, cx = h//2, w//2
        rc = r[cy-q:cy+q, cx-q:cx+q]
        gc_ = g[cy-q:cy+q, cx-q:cx+q]
        bc = b[cy-q:cy+q, cx-q:cx+q]

        features[name] = {
            'R_mean': rc.mean(),
            'G_mean': gc_.mean(),
            'B_mean': bc.mean(),
            'bright': (rc + gc_ + bc).mean() / 3,
            'R-G': (rc - gc_).mean(),
            'R-B': (rc - bc).mean(),
            'G-B': (gc_ - bc).mean(),
            'R/G': (rc / np.maximum(gc_, 1)).mean(),
            'R/B': (rc / np.maximum(bc, 1)).mean(),
            'G/B': (gc_ / np.maximum(bc, 1)).mean(),
            'saturation': ((np.maximum(np.maximum(rc, gc_), bc) -
                           np.minimum(np.minimum(rc, gc_), bc))).mean(),
            # Which channel dominates
            'R_frac': rc.mean() / max((rc + gc_ + bc).mean(), 1),
            'G_frac': gc_.mean() / max((rc + gc_ + bc).mean(), 1),
            'B_frac': bc.mean() / max((rc + gc_ + bc).mean(), 1),
        }

    # Print feature table
    feat_names = list(next(iter(features.values())).keys())
    print(f"\n  {'Feature':12s}", end="")
    for name in sorted(features.keys()):
        print(f"  {name:>10s}", end="")
    print()
    print("  " + "-" * (12 + 12 * len(features)))

    for feat in feat_names:
        print(f"  {feat:12s}", end="")
        for name in sorted(features.keys()):
            print(f"  {features[name][feat]:10.2f}", end="")
        print()

    # === Find best discriminators ===
    # For practical detection, we need to distinguish:
    # 1. "Has dark square" (any icon) vs "Empty" (rarity bg only)
    # 2. Among icons: lock vs astral vs elixir
    print(f"\n  === BEST DISCRIMINATORS ===\n")

    # Group: icon slots vs empty slots
    icon_names = ["lock5", "lock4", "astral5", "astral4", "elixir"]
    empty_names = ["rarity5", "rarity4"]

    for feat in feat_names:
        icon_vals = [features[n][feat] for n in icon_names]
        empty_vals = [features[n][feat] for n in empty_names]
        icon_range = (min(icon_vals), max(icon_vals))
        empty_range = (min(empty_vals), max(empty_vals))
        gap = min(empty_vals) - max(icon_vals)
        if gap < 0:
            gap = min(icon_vals) - max(empty_vals)
            if gap < 0:
                gap_str = "OVERLAP"
            else:
                gap_str = f"gap={gap:.1f} (icon > empty)"
        else:
            gap_str = f"gap={gap:.1f} (empty > icon)"
        print(f"  {feat:12s}: icon=[{icon_range[0]:.1f}, {icon_range[1]:.1f}]  "
              f"empty=[{empty_range[0]:.1f}, {empty_range[1]:.1f}]  {gap_str}")

    # Between icon types
    print(f"\n  === ICON TYPE DISCRIMINATION ===\n")
    lock_names = ["lock5", "lock4"]
    astral_names = ["astral5", "astral4"]
    elixir_names = ["elixir"]

    for feat in feat_names:
        lv = [features[n][feat] for n in lock_names]
        av = [features[n][feat] for n in astral_names]
        ev = [features[n][feat] for n in elixir_names]
        print(f"  {feat:12s}: lock=[{min(lv):.1f},{max(lv):.1f}]  "
              f"astral=[{min(av):.1f},{max(av):.1f}]  "
              f"elixir=[{min(ev):.1f},{max(ev):.1f}]")


if __name__ == "__main__":
    for name in sorted(crops.keys()):
        analyze_crop(name, crops[name])

    compare_backgrounds()
    analyze_dark_square_signature()
    analyze_icon_colors_detail()
