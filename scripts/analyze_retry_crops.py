"""Analyze retry crop images from grid_scan debug dumps.

Examines brightness, contrast, and tests preprocessing techniques
to understand why OCR fails on bright animated backgrounds.

Usage:
    python scripts/analyze_retry_crops.py
"""
import os
import sys
from pathlib import Path
from collections import defaultdict

try:
    from PIL import Image
    import numpy as np
except ImportError:
    print("pip install Pillow numpy")
    sys.exit(1)

GRID_DIR = Path("target/debug/debug_images/grid_scan")

def analyze_image(path):
    """Return basic stats about an image."""
    img = Image.open(path).convert("RGB")
    arr = np.array(img, dtype=np.float32)
    r, g, b = arr[:,:,0], arr[:,:,1], arr[:,:,2]
    brightness = arr.mean()
    # Contrast: std dev of luminance
    lum = 0.299 * r + 0.587 * g + 0.114 * b
    contrast = lum.std()
    # Color dominance
    r_mean, g_mean, b_mean = r.mean(), g.mean(), b.mean()
    return {
        "brightness": brightness,
        "contrast": contrast,
        "r": r_mean, "g": g_mean, "b": b_mean,
        "size": img.size,
    }

def preprocess_methods(img_path):
    """Try various preprocessing and return images."""
    img = Image.open(img_path).convert("RGB")
    arr = np.array(img, dtype=np.float32)
    results = {}

    # 1. Raw (baseline)
    results["raw"] = img

    # 2. Grayscale
    gray = np.dot(arr, [0.299, 0.587, 0.114])
    results["gray"] = Image.fromarray(gray.astype(np.uint8), "L")

    # 3. Invert (white text on bright bg -> dark text on dark bg... not great)
    # Better: extract text by finding dark-ish pixels

    # 4. Blue channel only (background is blue, text is white —
    #    in blue channel, both are bright. Red channel: text is bright, bg less so)
    results["red_ch"] = Image.fromarray(arr[:,:,0].astype(np.uint8), "L")
    results["green_ch"] = Image.fromarray(arr[:,:,1].astype(np.uint8), "L")
    results["blue_ch"] = Image.fromarray(arr[:,:,2].astype(np.uint8), "L")

    # 5. (R - B) difference — text is white (R≈B), background is blue (B>>R)
    # So R-B: text≈0, background<0 → invert: text bright, background dark
    diff = arr[:,:,0] - arr[:,:,2]  # R - B
    diff_norm = ((diff - diff.min()) / (diff.max() - diff.min() + 1e-6) * 255)
    results["r_minus_b"] = Image.fromarray(diff_norm.astype(np.uint8), "L")

    # 6. Max channel (white text = high in all channels, blue bg = high only in B)
    # min channel: white text = high in all, blue bg = low in R
    min_ch = arr.min(axis=2)
    results["min_channel"] = Image.fromarray(min_ch.astype(np.uint8), "L")

    # 7. Saturation-based: white text has low saturation, blue bg has high
    max_ch = arr.max(axis=2)
    sat = (max_ch - min_ch) / (max_ch + 1e-6) * 255
    results["low_sat"] = Image.fromarray((255 - sat).clip(0, 255).astype(np.uint8), "L")

    # 8. Adaptive threshold on min_channel
    from PIL import ImageFilter
    min_img = Image.fromarray(min_ch.astype(np.uint8), "L")
    # Local mean via box blur
    blurred = min_img.filter(ImageFilter.BoxBlur(15))
    min_arr = np.array(min_img, dtype=np.float32)
    blur_arr = np.array(blurred, dtype=np.float32)
    # Pixels brighter than local mean = text
    adaptive = ((min_arr > blur_arr - 10) * 255).astype(np.uint8)
    results["adaptive_min"] = Image.fromarray(adaptive, "L")

    # 9. CLAHE-like contrast enhancement on min channel
    # Simple: histogram equalization
    from PIL import ImageOps
    results["equalized_min"] = ImageOps.equalize(min_img)

    # 10. Sharpen then min channel
    sharpened = img.filter(ImageFilter.SHARPEN)
    sharp_arr = np.array(sharpened, dtype=np.float32)
    sharp_min = sharp_arr.min(axis=2)
    results["sharp_min"] = Image.fromarray(sharp_min.astype(np.uint8), "L")

    return results

def main():
    if not GRID_DIR.exists():
        print(f"No debug images at {GRID_DIR}")
        return

    # Group files by cell position
    cells = defaultdict(dict)
    for f in sorted(GRID_DIR.glob("p*_r*_c*_*.png")):
        name = f.stem  # e.g. p0_r0_c0_sub0
        parts = name.rsplit("_", 1)
        if len(parts) == 2:
            cell_key, field = parts
            cells[cell_key][field] = f

    print(f"Found {len(cells)} cell positions with crops")

    # Analyze brightness/contrast per cell
    bright_cells = []
    for cell_key in sorted(cells.keys()):
        fields = cells[cell_key]
        if "sub0" not in fields:
            continue
        stats = analyze_image(fields["sub0"])
        bright_cells.append((cell_key, stats))

    # Sort by brightness (highest = most problematic)
    bright_cells.sort(key=lambda x: -x[1]["brightness"])

    print(f"\n=== Top 10 brightest sub0 crops (most problematic) ===")
    for cell_key, stats in bright_cells[:10]:
        print(f"  {cell_key}: brightness={stats['brightness']:.0f} contrast={stats['contrast']:.0f} "
              f"RGB=({stats['r']:.0f},{stats['g']:.0f},{stats['b']:.0f})")

    print(f"\n=== Bottom 5 (easiest) ===")
    for cell_key, stats in bright_cells[-5:]:
        print(f"  {cell_key}: brightness={stats['brightness']:.0f} contrast={stats['contrast']:.0f} "
              f"RGB=({stats['r']:.0f},{stats['g']:.0f},{stats['b']:.0f})")

    # Save preprocessed versions of worst case
    if bright_cells:
        worst = bright_cells[0][0]
        print(f"\n=== Preprocessing worst case: {worst} ===")
        out_dir = GRID_DIR / "preprocess_test"
        out_dir.mkdir(exist_ok=True)

        for field_name in ["sub0", "sub1", "sub2", "sub3", "main", "set"]:
            if field_name not in cells[worst]:
                continue
            img_path = cells[worst][field_name]
            preprocessed = preprocess_methods(img_path)
            for method_name, result_img in preprocessed.items():
                out_path = out_dir / f"{worst}_{field_name}_{method_name}.png"
                result_img.save(out_path)
            print(f"  Saved {len(preprocessed)} preprocessed versions of {field_name}")

        # Also do a moderate brightness cell for comparison
        mid_idx = len(bright_cells) // 2
        mid = bright_cells[mid_idx][0]
        print(f"\n=== Preprocessing mid case: {mid} ===")
        for field_name in ["sub0", "sub1", "main"]:
            if field_name not in cells[mid]:
                continue
            img_path = cells[mid][field_name]
            preprocessed = preprocess_methods(img_path)
            for method_name, result_img in preprocessed.items():
                out_path = out_dir / f"{mid}_{field_name}_{method_name}.png"
                result_img.save(out_path)
            print(f"  Saved {len(preprocessed)} preprocessed versions of {field_name}")

    print(f"\nPreprocessed images saved to {out_dir}")
    print("Examine them to pick the best method for OCR.")

if __name__ == "__main__":
    main()
