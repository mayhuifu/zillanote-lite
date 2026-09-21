"""Draws the ZillaNote icon: an ear in violet light on graphite.

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


# Warm paper and terracotta: the app sits next to Claude and should look at home there.
PAPER_LIGHT, PAPER_DEEP = (58, 58, 68), (28, 28, 34)  # graphite
VIOLET_LIGHT, VIOLET_DEEP = (199, 187, 247), (132, 104, 232)
VIOLET_SHADOW = (8, 6, 20)


def background():
    """Warm ivory, a little lighter toward the upper left, inside the macOS icon shape, with
    a hairline edge so the light shape holds against a light Dock or Finder window."""
    inset, radius = 100 * SCALE, 190 * SCALE
    box = (inset, inset, BIG - inset, BIG - inset)
    shape = Image.new("L", (BIG, BIG), 0)
    ImageDraw.Draw(shape).rounded_rectangle(box, radius, fill=255)

    small = 256  # the gradient is smooth, so it can be computed small and enlarged
    gradient = Image.new("RGB", (small, small))
    pixels = gradient.load()
    for y in range(small):
        for x in range(small):
            t = min(1.0, math.hypot(x / small - 0.30, y / small - 0.22) / 0.95) ** 1.3
            pixels[x, y] = tuple(round(a + (b - a) * t) for a, b in zip(PAPER_LIGHT, PAPER_DEEP))

    icon = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
    icon.paste(gradient.resize((BIG, BIG), Image.BICUBIC), (0, 0), shape)
    edge = Image.new("L", (BIG, BIG), 0)
    ImageDraw.Draw(edge).rounded_rectangle(box, radius, outline=255, width=3 * SCALE)
    icon.paste(Image.new("RGBA", (BIG, BIG), (*VIOLET_SHADOW, 255)), (0, 0), edge.point(lambda v: v * 0.16))
    return icon, shape


def lay(icon, mask, top=VIOLET_LIGHT, bottom=VIOLET_DEEP, shadow=True):
    """Fills `mask` with a terracotta gradient over a soft warm shadow."""
    if shadow:
        dark = ImageChops.offset(mask, 0, 8 * SCALE).filter(ImageFilter.GaussianBlur(12 * SCALE))
        icon.paste(Image.new("RGBA", (BIG, BIG), (*VIOLET_SHADOW, 255)), (0, 0), dark.point(lambda v: v * 0.28))
    column = Image.new("RGB", (1, BIG))
    lo, hi = 240 * SCALE, 800 * SCALE
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


def line(draw, curves, width):
    """One brush line through several curves; `width` runs from 0 to 1 over the whole line,
    so the joins between the curves do not show."""
    for index, points in enumerate(curves):
        stroke(draw, points, lambda t, index=index: width((index + t) / len(curves)))


def ear_mask():
    """An ear, in two strokes of a brush: the rim, which comes up from the side of the head,
    swells over the top and runs out in the curl of the lobe; and the fold inside it."""
    mask = Image.new("L", (BIG, BIG), 0)
    draw = ImageDraw.Draw(mask)
    rim = [
        [(348, 514), (324, 324), (428, 232), (536, 234)],
        [(536, 234), (652, 237), (718, 334), (704, 446)],
        [(704, 446), (692, 550), (588, 590), (564, 684)],
        [(564, 684), (542, 770), (446, 794), (394, 736)],
    ]
    fold = [
        [(604, 464), (616, 366), (496, 324), (452, 414)],
        [(452, 414), (428, 468), (524, 502), (512, 574)],
    ]
    line(draw, rim, lambda u: 26 + 54 * math.sin(math.pi * min(1.0, u * 1.08)) ** 0.7)
    line(draw, fold, lambda u: 14 + 38 * math.sin(math.pi * (0.12 + 0.88 * u)) ** 0.8)
    return mask


def ear(icon):
    lay(icon, ear_mask())


def tray(size=44):
    """The mark alone, black on nothing, for the menu bar: macOS tints a template image to
    suit a light or a dark bar. Drawn a little heavier, so the fine ends survive 22 points."""
    mask = ear_mask().resize((SIZE, SIZE), Image.LANCZOS).filter(ImageFilter.MaxFilter(15))
    left, top, right, bottom = mask.getbbox()
    side = max(right - left, bottom - top) * 1.12
    cx, cy = (left + right) / 2, (top + bottom) / 2
    glyph = mask.crop((int(cx - side / 2), int(cy - side / 2), int(cx + side / 2), int(cy + side / 2)))
    image = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    image.putalpha(glyph.resize((size, size), Image.LANCZOS))
    return image


CONCEPTS = {"ear": ear}


def render(concept):
    icon, shape = background()
    CONCEPTS[concept](icon)
    # A hairline of light along the top edge finishes the shape.
    rim = ImageChops.subtract(shape, ImageChops.offset(shape, 0, 3 * SCALE))
    icon.paste(Image.new("RGBA", (BIG, BIG), (255, 255, 255, 255)), (0, 0), rim.point(lambda v: v * 0.22))
    return icon.resize((SIZE, SIZE), Image.LANCZOS)


def main():
    concept = sys.argv[1] if len(sys.argv) > 1 else "ear"
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
