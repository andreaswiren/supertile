#!/usr/bin/env python3
"""Generate supertile.ico / supertile-paused.ico from the SVG mark geometry.

The SVGs in assets/icons are the design source of truth. Windows needs a
multi-resolution .ico for the tray, the taskbar and Alt-Tab, and there is no
SVG rasteriser in the build environment, so this script redraws the *same*
geometry with Pillow and emits the icon.

The geometry constants below mirror assets/icons/supertile.svg exactly. If you
change one, change the other -- verify_matches_svg() checks the rectangles
still agree, and CI runs it.

Usage:  python scripts/make-icons.py
"""

import os
import re
import sys

try:
    from PIL import Image, ImageDraw
except ImportError:
    sys.exit("Pillow is required: python -m pip install Pillow")

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ICONS = os.path.join(ROOT, "assets", "icons")

# --- Geometry, in the SVG's 24x24 design space -----------------------------
CANVAS = 24.0
BADGE = (0.5, 0.5, 23.5, 23.5)   # rounded-square background
BADGE_RADIUS = 5.2
# Master + stack: one tall tile, two stacked. Mirrors the SVG.
TILES = [
    (4.0, 4.0, 11.0, 20.0),      # left, full height
    (13.0, 4.0, 20.0, 11.0),     # right top
    (13.0, 13.0, 20.0, 20.0),    # right bottom
]
TILE_RADIUS = 1.5

# A saturated indigo stays legible on both light and dark Windows 11 taskbars;
# a single-colour glyph would vanish against one of them.
BADGE_COLOR = (58, 106, 224, 255)        # #3A6AE0
BADGE_COLOR_PAUSED = (108, 114, 133, 255)  # #6C7285, desaturated
TILE_COLOR = (255, 255, 255, 255)

# Windows picks the nearest size for the tray (16 at 100%, 20 at 125%,
# 24 at 150%), the taskbar (24-40) and Alt-Tab / file properties (32-256).
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
SUPERSAMPLE = 8


def draw_mark(size, paused=False):
    """Render the mark at `size` px, supersampled then box-filtered down."""
    s = size * SUPERSAMPLE
    scale = s / CANVAS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    def px(v):
        return v * scale

    d.rounded_rectangle(
        [px(BADGE[0]), px(BADGE[1]), px(BADGE[2]), px(BADGE[3])],
        radius=px(BADGE_RADIUS),
        fill=BADGE_COLOR_PAUSED if paused else BADGE_COLOR,
    )

    tile_fill = TILE_COLOR if not paused else (255, 255, 255, 150)
    for (x0, y0, x1, y1) in TILES:
        d.rounded_rectangle(
            [px(x0), px(y0), px(x1), px(y1)], radius=px(TILE_RADIUS), fill=tile_fill
        )

    if paused:
        # Two bars over the lower-right, matching supertile-paused.svg.
        cx, cy, r = 17.0, 17.0, 5.6
        d.ellipse(
            [px(cx - r), px(cy - r), px(cx + r), px(cy + r)],
            fill=(28, 30, 38, 255),
        )
        for bx in (cx - 2.0, cx + 0.6):
            d.rounded_rectangle(
                [px(bx), px(cy - 2.7), px(bx + 1.4), px(cy + 2.7)],
                radius=px(0.7),
                fill=(255, 255, 255, 255),
            )

    return img.resize((size, size), Image.LANCZOS)


def build(path, paused=False):
    frames = [draw_mark(n, paused) for n in SIZES]
    # Pillow writes every supplied size into the .ico when they are passed as
    # append_images with the largest as the base.
    base = frames[-1]
    base.save(
        path,
        format="ICO",
        sizes=[(n, n) for n in SIZES],
        append_images=frames[:-1],
    )
    return path


def parse_svg_rects(svg_path):
    """Pull (x, y, w, h) out of every <rect> in an SVG, in document order."""
    text = open(svg_path, encoding="utf-8").read()
    out = []
    for tag in re.findall(r"<rect[^>]*>", text):
        def attr(name):
            m = re.search(rf'{name}="([-0-9.]+)"', tag)
            return float(m.group(1)) if m else None
        x, y, w, h = attr("x"), attr("y"), attr("width"), attr("height")
        if None not in (x, y, w, h):
            out.append((x, y, w, h))
    return out


def verify_matches_svg():
    """Fail loudly if the SVG source and this script have drifted apart."""
    svg = os.path.join(ICONS, "supertile.svg")
    rects = parse_svg_rects(svg)
    # First rect is the badge, the next three are the tiles.
    if len(rects) < 4:
        raise SystemExit(f"{svg}: expected a badge plus 3 tiles, found {len(rects)}")
    badge, tiles = rects[0], rects[1:4]

    want_badge = (BADGE[0], BADGE[1], BADGE[2] - BADGE[0], BADGE[3] - BADGE[1])
    if badge != want_badge:
        raise SystemExit(f"badge drift: svg={badge} script={want_badge}")

    want_tiles = [(x0, y0, x1 - x0, y1 - y0) for (x0, y0, x1, y1) in TILES]
    if tiles != want_tiles:
        raise SystemExit(f"tile drift:\n  svg   ={tiles}\n  script={want_tiles}")
    print("geometry matches assets/icons/supertile.svg")


if __name__ == "__main__":
    verify_matches_svg()
    for name, paused in (("supertile.ico", False), ("supertile-paused.ico", True)):
        p = build(os.path.join(ICONS, name), paused)
        print(f"wrote {os.path.relpath(p, ROOT)} ({os.path.getsize(p)} bytes)")
