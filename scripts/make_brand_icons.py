"""Builds assets/app.ico from the RustBackup application tile (Arche direction).

Mirrors assets/brand/app-tile.svg (64x64 design grid): ink rounded square,
chalk arch on a ground line, brass hull. Each size is drawn at 16x and
downsampled; sizes <= 24 px use a heavier stroke so the arch stays legible.

Usage (from the repository root):
    uv run --no-project --with pillow python scripts/make_brand_icons.py [preview_dir]
"""

import sys
from pathlib import Path

from PIL import Image, ImageDraw

INK = (0x1B, 0x24, 0x20, 255)
CHALK = (0xEC, 0xEA, 0xE2, 255)
BRASS = (0xD2, 0xB5, 0x77, 255)
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
SUPER = 16


def draw_tile(size: int) -> Image.Image:
    k = size * SUPER / 64.0  # design unit -> supersampled pixels

    def p(x: float, y: float) -> tuple[float, float]:
        return (x * k, y * k)

    big = Image.new("RGBA", (size * SUPER, size * SUPER), (0, 0, 0, 0))
    d = ImageDraw.Draw(big)
    d.rounded_rectangle([p(2, 2), p(62, 62)], radius=14 * k, fill=INK)

    stroke = 5.0 if size >= 32 else 6.5
    w = max(1, round(stroke * k))
    r_cap = stroke * k / 2

    def line(a: tuple[float, float], b: tuple[float, float]) -> None:
        d.line([p(*a), p(*b)], fill=CHALK, width=w)
        for x, y in (a, b):
            cx, cy = p(x, y)
            d.ellipse([cx - r_cap, cy - r_cap, cx + r_cap, cy + r_cap], fill=CHALK)

    # Arch: two posts and a half circle (center 32,30, radius 13).
    line((19, 47), (19, 30))
    line((45, 30), (45, 47))
    cx, cy, r = 32 * k, 30 * k, 13 * k
    half = w / 2
    d.arc([cx - r - half, cy - r - half, cx + r + half, cy + r + half], 180, 360, fill=CHALK, width=w)
    # Ground line.
    line((12, 47), (52, 47))
    # Brass hull.
    d.polygon([p(24.5, 37), p(39.5, 37), p(36.5, 42.5), p(27.5, 42.5)], fill=BRASS)

    return big.resize((size, size), Image.LANCZOS)


def main() -> None:
    root = Path(__file__).resolve().parent.parent
    images = [draw_tile(s) for s in SIZES]
    out = root / "assets" / "app.ico"
    images[-1].save(out, format="ICO", sizes=[(s, s) for s in SIZES], append_images=images[:-1])
    print(f"wrote {out} ({', '.join(str(s) for s in SIZES)} px)")
    if len(sys.argv) > 1:
        preview = Path(sys.argv[1])
        preview.mkdir(parents=True, exist_ok=True)
        for s, im in zip(SIZES, images):
            im.save(preview / f"tile-{s}.png")
        print(f"previews in {preview}")


if __name__ == "__main__":
    main()
