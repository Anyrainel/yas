"""
Measure FFT grid calibration consistency across ALL available images.
Reports: distribution of detected (gx, gy), error vs GT, and margin analysis.
"""
import json, os
import numpy as np
from PIL import Image
from multiprocessing import Pool, cpu_count
from star_grid_calibrate import (
    calibrate_fft, get_gt_gxy, GX, GY, OX, OY, CARD_W, CARD_H,
    COLS, ROWS, ITEMS_PER_PAGE, LOCK_DX, LOCK_DY, SLOT_SPACING, CROP_HALF
)

ART_DIR = "F:/Codes/genshin/yas/target/release/debug_images/artifacts"
WPN_DIR = "F:/Codes/genshin/yas/target/release/debug_images/weapons"
ART_GT_FILE = "F:/Codes/genshin/yas/scripts/grid_ground_truth.json"

# How far off can gx/gy be before icon detection fails?
# Icon sampling area is CROP_HALF=8px (±8px box).
# If offset error > ~8px at 4K (4px at 1080p), the sampling box
# may miss the icon entirely.
TOLERANCE_4K = 8.0  # pixels at 4K


def process_art(args):
    idx, total_art, art_lock = args
    src = os.path.join(ART_DIR, f"{idx:04d}", "full.png")
    if not os.path.exists(src):
        return None
    img_np = np.array(Image.open(src))
    gt_gx, gt_gy = get_gt_gxy(img_np, idx, total_art, art_lock)
    fft_gx, fft_gy, score = calibrate_fft(img_np, "artifact")
    return {
        'idx': idx, 'gt_gx': gt_gx, 'gt_gy': gt_gy,
        'fft_gx': fft_gx, 'fft_gy': fft_gy, 'score': score,
        'dx': fft_gx - gt_gx, 'dy': fft_gy - gt_gy,
    }


def process_wpn(idx):
    src = os.path.join(WPN_DIR, f"{idx:04d}", "full.png")
    if not os.path.exists(src):
        return None
    img_np = np.array(Image.open(src))
    fft_gx, fft_gy, score = calibrate_fft(img_np, "weapon")
    return {'idx': idx, 'fft_gx': fft_gx, 'fft_gy': fft_gy, 'score': score}


def main():
    art_lock = {}
    if os.path.exists(ART_GT_FILE):
        with open(ART_GT_FILE) as f:
            for g in json.load(f)['items']:
                art_lock[g['idx']] = g['lock']
    total_art = max(art_lock.keys()) + 1 if art_lock else 0

    # ============================================================
    # Artifact: sample every 40th image (one per page) + extras
    # ============================================================
    art_indices = list(range(0, total_art, 40))  # first item of each page
    # Add some mid-page items too
    art_indices += list(range(20, total_art, 40))
    art_indices = sorted(set(i for i in art_indices if i < total_art))

    print(f"Testing {len(art_indices)} artifact images...")
    art_args = [(idx, total_art, art_lock) for idx in art_indices]

    with Pool(min(cpu_count(), 8)) as pool:
        art_results = [r for r in pool.map(process_art, art_args) if r is not None]

    dx = np.array([r['dx'] for r in art_results])
    dy = np.array([r['dy'] for r in art_results])
    dist = np.sqrt(dx**2 + dy**2)

    print(f"\n=== ARTIFACT vs GT ({len(art_results)} images) ===")
    print(f"  dx: mean={dx.mean():+.2f}  std={dx.std():.2f}  "
          f"min={dx.min():+.2f}  max={dx.max():+.2f}")
    print(f"  dy: mean={dy.mean():+.2f}  std={dy.std():.2f}  "
          f"min={dy.min():+.2f}  max={dy.max():+.2f}")
    print(f"  dist: mean={dist.mean():.2f}  p95={np.percentile(dist,95):.2f}  "
          f"max={dist.max():.2f}")
    print(f"  Within {TOLERANCE_4K}px: {np.sum(dist < TOLERANCE_4K)}/{len(dist)} "
          f"({np.sum(dist < TOLERANCE_4K)/len(dist)*100:.1f}%)")

    # Unique (gx, gy) values detected
    fft_gx_vals = sorted(set(r['fft_gx'] for r in art_results))
    fft_gy_vals = sorted(set(r['fft_gy'] for r in art_results))
    print(f"  Unique gx values: {fft_gx_vals}")
    print(f"  Unique gy values: {fft_gy_vals}")

    # Per-page consistency
    gt_gy_vals = sorted(set(round(r['gt_gy'], 1) for r in art_results))
    print(f"  GT gy values: {gt_gy_vals}")

    # Worst cases
    worst = sorted(art_results, key=lambda r: abs(r['dx'])**2 + abs(r['dy'])**2, reverse=True)
    print(f"\n  Worst 5:")
    for r in worst[:5]:
        print(f"    idx={r['idx']:4d}  dx={r['dx']:+.1f} dy={r['dy']:+.1f} "
              f"dist={np.sqrt(r['dx']**2+r['dy']**2):.1f}")

    # ============================================================
    # Weapon: sample every 40th + mid-page
    # ============================================================
    wpn_count = len([d for d in os.listdir(WPN_DIR) if d.isdigit()])
    wpn_indices = list(range(0, wpn_count, 40))
    wpn_indices += list(range(20, wpn_count, 40))
    wpn_indices = sorted(set(i for i in wpn_indices if i < wpn_count))

    print(f"\nTesting {len(wpn_indices)} weapon images...")

    with Pool(min(cpu_count(), 8)) as pool:
        wpn_results = [r for r in pool.map(process_wpn, wpn_indices) if r is not None]

    wpn_gx = np.array([r['fft_gx'] for r in wpn_results])
    wpn_gy = np.array([r['fft_gy'] for r in wpn_results])
    wpn_scores = np.array([r['score'] for r in wpn_results])

    print(f"\n=== WEAPON ({len(wpn_results)} images) ===")
    print(f"  gx: mean={wpn_gx.mean():.2f}  std={wpn_gx.std():.2f}  "
          f"min={wpn_gx.min():.1f}  max={wpn_gx.max():.1f}")
    print(f"  gy: mean={wpn_gy.mean():.2f}  std={wpn_gy.std():.2f}  "
          f"min={wpn_gy.min():.1f}  max={wpn_gy.max():.1f}")
    print(f"  score: mean={wpn_scores.mean():.0f}  std={wpn_scores.std():.0f}")

    # Unique values
    wpn_gx_uniq = sorted(set(round(v, 1) for v in wpn_gx))
    wpn_gy_uniq = sorted(set(round(v, 1) for v in wpn_gy))
    print(f"  Unique gx values: {wpn_gx_uniq}")
    print(f"  Unique gy values: {wpn_gy_uniq}")

    # gy distribution
    from collections import Counter
    gy_counts = Counter(round(v, 0) for v in wpn_gy)
    print(f"  gy distribution: {dict(sorted(gy_counts.items()))}")

    # ============================================================
    # Margin analysis
    # ============================================================
    print(f"\n=== MARGIN ANALYSIS ===")
    print(f"  Icon sampling box: ±{CROP_HALF}px at 4K = ±{CROP_HALF/2:.0f}px at 1080p")
    print(f"  Icon slot spacing: {SLOT_SPACING}px at 4K = {SLOT_SPACING/2:.1f}px at 1080p")

    if art_results:
        art_max_err = dist.max()
        art_margin = CROP_HALF - art_max_err
        print(f"\n  Artifact:")
        print(f"    Max error vs GT: {art_max_err:.1f}px at 4K ({art_max_err/2:.1f}px at 1080p)")
        print(f"    Remaining margin: {art_margin:.1f}px at 4K ({art_margin/2:.1f}px at 1080p)")
        print(f"    {'OK' if art_margin > 0 else 'DANGER'}: "
              f"{'safe' if art_margin > 2 else 'tight' if art_margin > 0 else 'EXCEEDS tolerance'}")

    if wpn_results:
        # For weapons, no GT — use spread as proxy
        wpn_gy_spread = wpn_gy.max() - wpn_gy.min()
        print(f"\n  Weapon:")
        print(f"    gx spread: {wpn_gx.max()-wpn_gx.min():.1f}px")
        print(f"    gy spread: {wpn_gy_spread:.1f}px")
        print(f"    If true gy is at median ({np.median(wpn_gy):.1f}), "
              f"max deviation: {wpn_gy_spread/2:.1f}px")


if __name__ == "__main__":
    main()
