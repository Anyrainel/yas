"""Crop test regions from a selection view screenshot to calibrate OCR coordinates.

Usage: python scripts/crop_test.py [screenshot_path]
Default: debug_images/set_filter_test/grid/final_screen.png
"""
import sys
from pathlib import Path
from PIL import Image

src = sys.argv[1] if len(sys.argv) > 1 else "debug_images/set_filter_test/grid/final_screen.png"
out_dir = Path("debug_images/crop_test")
out_dir.mkdir(parents=True, exist_ok=True)

# Clean old crops
for f in out_dir.iterdir():
    if f.is_file():
        f.unlink()

img = Image.open(src)
w, h = img.size
print(f"Image size: {w}x{h}")
scale = w / 1920.0
print(f"Scale factor: {scale}")

def crop_region(name, x, y, rw, rh):
    sx, sy = int(x * scale), int(y * scale)
    sw, sh = int(rw * scale), int(rh * scale)
    region = img.crop((sx, sy, sx + sw, sy + sh))
    path = out_dir / f"{name}.png"
    region.save(path)
    print(f"  {name}: ({x},{y},{rw},{rh}) -> {path}")

# Final proposed regions v6 (base 1920x1080)
# Pixel-measured from panel image at 2x:
#   Stars:  screen y=268, x=1476-1613
#   Level:  screen y=314-332, x=1477-1507
#   Sub0:   screen y=354-374, x=1474-1628
#   Sub1:   screen y=387-408, x=1474-1636
#   Sub2:   screen y=421-441, x=1474-1620
#   Sub3:   ~y=454-474 (extrapolated +33px)
#   Set:    ~y=487-507
print("\n=== Final proposed regions v10 ===")
crop_region("main_stat",   1440, 217, 250, 30)    # +10x
crop_region("level",       1443, 310, 100, 26)    # -2x
# Star pixel test points: star4 center=(1578,277), star5 center=(1612,277)
crop_region("star4_check", 1568, 270, 21, 21)     # ±10px around star4 (1578,280)
crop_region("star5_check", 1601, 270, 21, 21)     # ±10px around star5 (1611,280)
crop_region("sub0",        1460, 349, 256, 30)    # +10x
crop_region("sub1",        1460, 383, 256, 30)    # +34
crop_region("sub2",        1460, 417, 256, 30)    # +34
crop_region("sub3",        1460, 451, 336, 30)    # +34, wider for (待激活)
crop_region("set_name",    1430, 489, 300, 30)    # +10x

print("\nDone! Check debug_images/crop_test/")
