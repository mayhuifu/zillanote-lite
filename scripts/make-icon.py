"""Draws the ZillaNote icon: a folded-ribbon "Z" on deep blue.

    python3 scripts/make-icon.py            # needs Pillow
    cd app && pnpm dlx @tauri-apps/cli@2.11.4 icon icons/source-1024.png -o icons

The Z is drawn from polygons rather than set in a typeface, so the mark is ours.
"""

from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

SIZE = 1024
SCALE = 4  # drawn large, then reduced, for smooth edges
ROOT = Path(__file__).resolve().parent.parent

DEEP_TOP = (22, 58, 140)
DEEP_BOTTOM = (8, 22, 66)
WHITE = (255, 255, 255)
FOLD_LIGHT = (196, 218, 255)
FOLD_DARK = (120, 160, 235)


def s(points):
    return [(x * SCALE, y * SCALE) for x, y in points]


def vertical_gradient(size, top, bottom):
    column = Image.new("RGB", (1, size))
    for y in range(size):
        t = y / (size - 1)
        column.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)))
    return column.resize((size, size))


def diagonal_gradient(size, start, end, box):
    """Light at the top right of `box`, darker toward its bottom left: the ribbon's fold."""
    x0, y0, x1, y1 = box
    image = Image.new("RGB", (size, size), start)
    pixels = image.load()
    for y in range(y0, y1):
        for x in range(x0, x1):
            t = ((x1 - x) / (x1 - x0) + (y - y0) / (y1 - y0)) / 2
            pixels[x, y] = tuple(round(a + (b - a) * t) for a, b in zip(start, end))
    return image


def main():
    big = SIZE * SCALE

    # macOS icon shape: a rounded square inset from the canvas.
    inset, radius = 100 * SCALE, 186 * SCALE
    shape = Image.new("L", (big, big), 0)
    ImageDraw.Draw(shape).rounded_rectangle((inset, inset, big - inset, big - inset), radius, fill=255)

    icon = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    icon.paste(vertical_gradient(big, DEEP_TOP, DEEP_BOTTOM), (0, 0), shape)

    # A soft highlight across the top, like light on glass.
    glow = Image.new("L", (big, big), 0)
    ImageDraw.Draw(glow).ellipse((-big // 4, -big // 2, big + big // 4, big // 2), fill=26)
    glow = ImageChops.multiply(glow.filter(ImageFilter.GaussianBlur(60 * SCALE)), shape)
    icon.paste(Image.new("RGBA", (big, big), (255, 255, 255, 255)), (0, 0), glow)

    # The Z as one ribbon: two white bars with slanted ends and a folded diagonal between.
    top_bar = s([(306, 292), (742, 292), (704, 392), (282, 392)])
    diagonal = s([(566, 392), (704, 392), (458, 632), (320, 632)])
    bottom_bar = s([(320, 632), (742, 632), (718, 732), (282, 732)])

    fold_mask = Image.new("L", (big, big), 0)
    ImageDraw.Draw(fold_mask).polygon(diagonal, fill=255)
    fold = diagonal_gradient(big, FOLD_LIGHT, FOLD_DARK, (300 * SCALE, 392 * SCALE, 720 * SCALE, 632 * SCALE))
    icon.paste(fold, (0, 0), fold_mask)

    # A shadow where each bar lies over the fold gives the ribbon its depth.
    shadow = Image.new("L", (big, big), 0)
    shadow_draw = ImageDraw.Draw(shadow)
    shadow_draw.polygon(s([(282, 392), (704, 392), (700, 412), (282, 412)]), fill=110)
    shadow_draw.polygon(s([(320, 612), (742, 612), (742, 632), (320, 632)]), fill=110)
    shadow = ImageChops.multiply(shadow.filter(ImageFilter.GaussianBlur(7 * SCALE)), fold_mask)
    icon.paste(Image.new("RGBA", (big, big), (6, 18, 60, 255)), (0, 0), shadow)

    draw = ImageDraw.Draw(icon)
    draw.polygon(top_bar, fill=WHITE)
    draw.polygon(bottom_bar, fill=WHITE)

    icon = icon.resize((SIZE, SIZE), Image.LANCZOS)
    icon.save(ROOT / "app" / "icons" / "source-1024.png")

    # The page shows the mark without the transparent margin.
    mark = icon.crop((100, 100, SIZE - 100, SIZE - 100)).resize((128, 128), Image.LANCZOS)
    mark.save(ROOT / "ui" / "logo.png")
    print("wrote app/icons/source-1024.png and ui/logo.png")


if __name__ == "__main__":
    main()
