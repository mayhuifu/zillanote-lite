"""Draws the ZillaNote icon.

    python3 scripts/make-icon.py                # needs Pillow
    cd app && pnpm dlx @tauri-apps/cli@2.11.4 icon icons/source-1024.png -o icons

Everything is drawn from curves and shapes rather than set in a typeface, so the mark is ours.
"""

import math
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

SIZE = 1024
SCALE = 4  # drawn large, then reduced, for smooth edges
BIG = SIZE * SCALE
ROOT = Path(__file__).resolve().parent.parent


def background():
    """Deep blue, a little lighter toward the upper left, inside the macOS icon shape."""
    inset, radius = 100 * SCALE, 190 * SCALE
    shape = Image.new("L", (BIG, BIG), 0)
    ImageDraw.Draw(shape).rounded_rectangle((inset, inset, BIG - inset, BIG - inset), radius, fill=255)

    small = 256  # the gradient is smooth, so it can be computed small and enlarged
    gradient = Image.new("RGB", (small, small))
    pixels = gradient.load()
    inner, outer = (30, 84, 190), (5, 14, 48)
    for y in range(small):
        for x in range(small):
            t = min(1.0, math.hypot(x / small - 0.32, y / small - 0.24) / 0.95) ** 1.15
            pixels[x, y] = tuple(round(a + (b - a) * t) for a, b in zip(inner, outer))

    icon = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
    icon.paste(gradient.resize((BIG, BIG), Image.BICUBIC), (0, 0), shape)
    return icon, shape


def lay(icon, mask, top=(255, 255, 255), bottom=(198, 220, 255), shadow=True):
    """Fills `mask` with a soft white-to-ice gradient over a blurred shadow."""
    if shadow:
        dark = ImageChops.offset(mask, 0, 10 * SCALE).filter(ImageFilter.GaussianBlur(14 * SCALE))
        icon.paste(Image.new("RGBA", (BIG, BIG), (2, 8, 36, 255)), (0, 0), dark.point(lambda v: v * 0.55))
    column = Image.new("RGB", (1, BIG))
    lo, hi = 260 * SCALE, 780 * SCALE
    for y in range(BIG):
        t = min(1.0, max(0.0, (y - lo) / (hi - lo)))
        column.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)))
    icon.paste(column.resize((BIG, BIG)), (0, 0), mask)


def bezier(p0, p1, p2, p3, steps=160):
    for i in range(steps + 1):
        t = i / steps
        a, b, c, d = (1 - t) ** 3, 3 * (1 - t) ** 2 * t, 3 * (1 - t) * t**2, t**3
        x = a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0]
        y = a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1]
        dx = 3 * ((1 - t) ** 2 * (p1[0] - p0[0]) + 2 * (1 - t) * t * (p2[0] - p1[0]) + t**2 * (p3[0] - p2[0]))
        dy = 3 * ((1 - t) ** 2 * (p1[1] - p0[1]) + 2 * (1 - t) * t * (p2[1] - p1[1]) + t**2 * (p3[1] - p2[1]))
        yield t, x, y, dx, dy


def stroke(draw, points, width):
    """A brush stroke: a curve whose thickness changes along its length."""
    left, right = [], []
    for t, x, y, dx, dy in bezier(*points):
        length = math.hypot(dx, dy) or 1.0
        half = width(t) / 2
        nx, ny = -dy / length * half, dx / length * half
        left.append(((x + nx) * SCALE, (y + ny) * SCALE))
        right.append(((x - nx) * SCALE, (y - ny) * SCALE))
    draw.polygon(left + right[::-1], fill=255)
    # Round both ends, so a stroke never finishes in a sharp wedge or a square cut.
    for (x, y), t in ((points[0], 0.0), (points[3], 1.0)):
        r = width(t) / 2 * SCALE
        draw.ellipse([x * SCALE - r, y * SCALE - r, x * SCALE + r, y * SCALE + r], fill=255)


def z_mask():
    """A pen-written Z: full horizontals, a lighter diagonal tucked into them, and a tail
    that sweeps away to the right."""
    mask = Image.new("L", (BIG, BIG), 0)
    draw = ImageDraw.Draw(mask)
    # The top stroke enters fine and ends full, where the diagonal leaves it; the bottom
    # stroke starts full, where the diagonal arrives, and runs out to a point.
    stroke(draw, [(258, 392), (352, 278), (566, 356), (736, 312)], lambda t: 22 + 82 * math.sin(math.pi * 0.62 * t) ** 0.8)
    stroke(draw, [(728, 318), (626, 446), (430, 596), (312, 712)], lambda t: 40 + 34 * math.sin(math.pi * t))
    stroke(draw, [(304, 718), (446, 646), (648, 776), (852, 636)], lambda t: 6 + 102 * math.sin(math.pi * (0.30 + 0.70 * t)) ** 0.8)
    return mask


def swash(icon):
    lay(icon, z_mask())


def tray(size=44):
    """The mark alone, black on nothing, for the menu bar: macOS tints a template image to
    suit a light or a dark bar. Drawn a little heavier, so the fine ends survive 22 points."""
    mask = z_mask().resize((SIZE, SIZE), Image.LANCZOS).filter(ImageFilter.MaxFilter(15))
    left, top, right, bottom = mask.getbbox()
    side = max(right - left, bottom - top) * 1.12
    cx, cy = (left + right) / 2, (top + bottom) / 2
    glyph = mask.crop((int(cx - side / 2), int(cy - side / 2), int(cx + side / 2), int(cy + side / 2)))
    image = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    image.putalpha(glyph.resize((size, size), Image.LANCZOS))
    return image


CONCEPTS = {"swash": swash}


def render(concept):
    icon, shape = background()
    CONCEPTS[concept](icon)
    # A hairline of light along the top edge finishes the shape.
    rim = ImageChops.subtract(shape, ImageChops.offset(shape, 0, 3 * SCALE))
    icon.paste(Image.new("RGBA", (BIG, BIG), (255, 255, 255, 255)), (0, 0), rim.point(lambda v: v * 0.22))
    return icon.resize((SIZE, SIZE), Image.LANCZOS)


def main():
    concept = sys.argv[1] if len(sys.argv) > 1 else "swash"
    if concept == "preview":
        sheet = Image.new("RGBA", (1200, 470 * len(CONCEPTS)), (238, 238, 240, 255))
        for row, name in enumerate(CONCEPTS):
            icon = render(name)
            sheet.alpha_composite(icon.resize((440, 440), Image.LANCZOS), (10, 15 + 470 * row))
            dark = Image.new("RGBA", (700, 440), (34, 36, 42, 255))
            x = 30
            for size in (256, 128, 64, 32, 16):
                dark.alpha_composite(icon.resize((size, size), Image.LANCZOS), (x, 40))
                x += size + 28
            sheet.alpha_composite(dark, (480, 15 + 470 * row))
        out = Path(sys.argv[2])
        sheet.convert("RGB").save(out)
        print(f"wrote {out}")
        return

    icon = render(concept)
    icon.save(ROOT / "app" / "icons" / "source-1024.png")
    # The pages show the mark without the transparent margin.
    icon.crop((100, 100, SIZE - 100, SIZE - 100)).resize((128, 128), Image.LANCZOS).save(ROOT / "ui" / "logo.png")
    tray().save(ROOT / "app" / "icons" / "tray.png")
    print(f"wrote app/icons/source-1024.png, app/icons/tray.png and ui/logo.png ({concept})")


if __name__ == "__main__":
    main()
