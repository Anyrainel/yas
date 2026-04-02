"""Quick diagnostic: look at actual brightness profiles to understand card edges."""
import numpy as np
from PIL import Image

img = np.array(Image.open("F:/Codes/genshin/yas/target/release/debug_images/artifacts/0000/full.png"))
h, w = img.shape[:2]
print(f"Image size: {w}x{h}")

# Horizontal brightness profile at mid-grid Y
# Average a band of rows for stability
band_y1, band_y2 = 400, 1400
band = img[band_y1:band_y2, :, :3].astype(np.float32)
col_bright = band.mean(axis=(0, 2))

print(f"\nHorizontal brightness (x=200..2600, sampling every 20px):")
for x in range(200, 2600, 20):
    bar = "#" * int(col_bright[x] / 5)
    print(f"  x={x:4d}: {col_bright[x]:6.1f} {bar}")

# Vertical brightness profile at a card center (x≈357)
strip_x1, strip_x2 = 340, 380
strip = img[:, strip_x1:strip_x2, :3].astype(np.float32)
row_bright = strip.mean(axis=(1, 2))

print(f"\nVertical brightness at x=360 (y=100..1900, every 20px):")
for y in range(100, 1900, 20):
    bar = "#" * int(row_bright[y] / 5)
    print(f"  y={y:4d}: {row_bright[y]:6.1f} {bar}")

# Look for column gaps: find local minima
print(f"\nColumn gap candidates (local minima in horizontal profile, x=200..2600):")
for x in range(250, 2550):
    if col_bright[x] < col_bright[x-5] - 3 and col_bright[x] < col_bright[x+5] - 3:
        # Check it's a significant dip
        left = col_bright[max(0,x-30):x-10].mean() if x > 40 else 0
        right = col_bright[x+10:x+30].mean() if x + 30 < w else 0
        depth = (left + right) / 2 - col_bright[x]
        if depth > 5:
            print(f"  x={x:4d}: brightness={col_bright[x]:.1f}  depth={depth:.1f}")
