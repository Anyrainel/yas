"""
Check how sharp the FFT correlation peak is.
For each sample, report the score at the peak and at ±1, ±5, ±10, ±20px offsets.
This tells us: is the peak sharp (good margin) or flat (ambiguous)?
"""
import os
import numpy as np
from PIL import Image
from scipy.signal import fftconvolve
from star_grid_calibrate import (
    compute_lightness, build_grid_template, GX, GY, OX, OY,
    CARD_W, CARD_H, COLS, ROWS
)

ART_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
WPN_DIR = "F:/Codes/genshin/yas/target/release/debug_images/weapons"


def get_corr_landscape(img_np, mode):
    """Get the full correlation map and peak location."""
    signal = compute_lightness(img_np)
    template, t_ox, t_oy = build_grid_template(*img_np.shape[:2])
    corr = fftconvolve(signal, template[::-1, ::-1], mode='full')
    t_h, t_w = template.shape

    if mode == "artifact":
        exp_gx, exp_gy = GX, GY
    else:
        exp_gx, exp_gy = GX, GY - 114

    exp_px = int(exp_gx + t_w - 1 - t_ox)
    exp_py = int(exp_gy + t_h - 1 - t_oy)

    search_r = 60
    py_min = max(0, exp_py - search_r)
    py_max = min(corr.shape[0], exp_py + search_r)
    px_min = max(0, exp_px - search_r)
    px_max = min(corr.shape[1], exp_px + search_r)

    sub_corr = corr[py_min:py_max, px_min:px_max]

    return sub_corr, px_min, py_min, t_ox, t_oy, t_w, t_h


def analyze_peak(sub_corr, label):
    """Analyze the correlation peak sharpness."""
    peak_idx = np.unravel_index(np.argmax(sub_corr), sub_corr.shape)
    peak_y, peak_x = peak_idx
    peak_val = sub_corr[peak_y, peak_x]

    print(f"\n  {label}:")
    print(f"    Peak at rel ({peak_x}, {peak_y}), score={peak_val:.0f}")

    # X profile at peak Y
    print(f"    X profile (at peak Y={peak_y}):")
    x_profile = sub_corr[peak_y, :]
    for dx in [-20, -10, -5, -2, -1, 0, 1, 2, 5, 10, 20]:
        px = peak_x + dx
        if 0 <= px < sub_corr.shape[1]:
            val = x_profile[px]
            diff = val - peak_val
            pct = diff / abs(peak_val) * 100 if peak_val != 0 else 0
            bar = '#' * max(0, int((val - x_profile.min()) / (peak_val - x_profile.min() + 1) * 30))
            print(f"      dx={dx:+3d}: {val:12.0f} ({pct:+.2f}%) {bar}")

    # Y profile at peak X
    print(f"    Y profile (at peak X={peak_x}):")
    y_profile = sub_corr[:, peak_x]
    for dy in [-20, -10, -5, -2, -1, 0, 1, 2, 5, 10, 20]:
        py = peak_y + dy
        if 0 <= py < sub_corr.shape[0]:
            val = y_profile[py]
            diff = val - peak_val
            pct = diff / abs(peak_val) * 100 if peak_val != 0 else 0
            bar = '#' * max(0, int((val - y_profile.min()) / (peak_val - y_profile.min() + 1) * 30))
            print(f"      dy={dy:+3d}: {val:12.0f} ({pct:+.2f}%) {bar}")

    # 2nd best peak (outside ±5px of main peak)
    masked = sub_corr.copy()
    y1 = max(0, peak_y - 5)
    y2 = min(sub_corr.shape[0], peak_y + 6)
    x1 = max(0, peak_x - 5)
    x2 = min(sub_corr.shape[1], peak_x + 6)
    masked[y1:y2, x1:x2] = -np.inf
    second_idx = np.unravel_index(np.argmax(masked), masked.shape)
    second_val = masked[second_idx]
    gap_pct = (peak_val - second_val) / abs(peak_val) * 100 if peak_val != 0 else 0
    print(f"    2nd peak at ({second_idx[1]}, {second_idx[0]}), score={second_val:.0f}")
    print(f"    Gap to 2nd: {peak_val - second_val:.0f} ({gap_pct:.2f}%)")


def main():
    samples = [
        ("artifact", ART_DIR, 0),
        ("artifact", ART_DIR, 1408),
        ("weapon", WPN_DIR, 0),
        ("weapon", WPN_DIR, 300),
        ("weapon", WPN_DIR, 500),
    ]

    for mode, base_dir, idx in samples:
        src = os.path.join(base_dir, f"{idx:04d}", "full.png")
        if not os.path.exists(src):
            continue
        img_np = np.array(Image.open(src))
        sub_corr, px_min, py_min, t_ox, t_oy, t_w, t_h = get_corr_landscape(img_np, mode)
        analyze_peak(sub_corr, f"{mode} idx={idx}")


if __name__ == "__main__":
    main()
