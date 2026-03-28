"""Generate icon.ico from icon_source.svg using resvg + Pillow.

Requires: resvg (cargo install resvg), Pillow
"""

import os
import subprocess
import tempfile
from PIL import Image


def main():
    assets_dir = os.path.dirname(os.path.abspath(__file__))
    svg_path = os.path.join(assets_dir, "icon_source.svg")
    ico_path = os.path.join(assets_dir, "icon.ico")
    png64_path = os.path.join(assets_dir, "icon_64.png")

    sizes = [256, 64, 48, 32, 16]
    images = []

    with tempfile.TemporaryDirectory() as tmpdir:
        for sz in sizes:
            png_path = os.path.join(tmpdir, f"icon_{sz}.png")
            subprocess.run(
                ["resvg", svg_path, png_path, "-w", str(sz), "-h", str(sz)],
                check=True,
            )
            images.append(Image.open(png_path).copy())

    # Save ico with all sizes
    images[0].save(
        ico_path,
        format="ICO",
        sizes=[(sz, sz) for sz in sizes],
        append_images=images[1:],
    )
    print(f"Saved {ico_path}")

    # Save 64px PNG for eframe window icon
    images[1].save(png64_path, format="PNG")
    print(f"Saved {png64_path}")


if __name__ == "__main__":
    main()
