#!/usr/bin/env python3
"""Generate SpiderClient's original monochrome brand assets.

The mark is drawn from scratch: a thin web thread, a compact spider, and eight
round-capped legs.  It is intentionally monochrome so it can be tinted by the
launcher and used on both light and dark surfaces.

Only build-time tooling is required:

    python -m pip install fonttools pillow

Usage (from the repository root):

    python tools/branding/generate_brand_assets.py
"""

from __future__ import annotations

import math
import struct
from pathlib import Path

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.ttLib import TTFont
from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "packages" / "oneclient_app"
ASSETS = APP / "assets"
ICONS = APP / "icons"
FONT_DIR = ASSETS / "fonts" / "Poppins"

INK = (11, 14, 17, 255)
SURFACE = (17, 21, 25, 255)
BORDER = (47, 54, 62, 255)
PAPER = (246, 248, 250, 255)
MUTED = (165, 174, 184, 255)


def spider_svg(size: int = 64, wink: bool = False) -> str:
    """A currentColor spider used for inline launcher branding.

    The wink frame keeps the body solid, which reads as closed eyes at small
    sizes without adding facial features that would clutter the mark.
    """
    eyes = "" if wink else "ZM28.6 27.4C27.8 27.4 27.2 28 27.2 28.8C27.2 29.6 27.8 30.2 28.6 30.2C29.4 30.2 30 29.6 30 28.8C30 28 29.4 27.4 28.6 27.4ZM35.4 27.4C34.6 27.4 34 28 34 28.8C34 29.6 34.6 30.2 35.4 30.2C36.2 30.2 36.8 29.6 36.8 28.8C36.8 28 36.2 27.4 35.4 27.4Z"
    return f'''<svg width="{size}" height="{size}" viewBox="0 0 64 64" fill="none" xmlns="http://www.w3.org/2000/svg">
  <path d="M32 0.75V17.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" opacity=".55"/>
  <g stroke="currentColor" stroke-width="2.15" stroke-linecap="round">
    <path d="M27.5 23.2C23.4 20.1 20.1 16.2 18.2 11.7"/>
    <path d="M26.8 27.4C21.9 26.7 17.5 24.9 13.6 22"/>
    <path d="M26.9 31.6C21.4 33.1 16.9 35.6 13.2 39.4"/>
    <path d="M28.4 35.6C24.4 40 21.8 45.2 20.9 50.8"/>
    <path d="M36.5 23.2C40.6 20.1 43.9 16.2 45.8 11.7"/>
    <path d="M37.2 27.4C42.1 26.7 46.5 24.9 50.4 22"/>
    <path d="M37.1 31.6C42.6 33.1 47.1 35.6 50.8 39.4"/>
    <path d="M35.6 35.6C39.6 40 42.2 45.2 43.1 50.8"/>
  </g>
  <path fill-rule="evenodd" clip-rule="evenodd" d="M32 17.6C38.6 17.6 43.9 22.9 43.9 29.5C43.9 36.1 38.6 41.4 32 41.4C25.4 41.4 20.1 36.1 20.1 29.5C20.1 22.9 25.4 17.6 32 17.6{eyes}" fill="currentColor"/>
  <path d="M32 17.6C34.3 17.6 36.2 19.5 36.2 21.8C36.2 23.7 34.9 25.2 33.2 25.7" stroke="currentColor" stroke-width="1.1" stroke-linecap="round" opacity=".45"/>
</svg>
'''


def glyph_run(text: str, font_path: Path, size: float, tracking: float) -> tuple[str, float]:
    font = TTFont(font_path)
    glyphs = font.getGlyphSet()
    cmap = font.getBestCmap()
    upm = font["head"].unitsPerEm
    scale = size / upm
    x = 0.0
    parts: list[str] = []
    for char in text:
        name = cmap.get(ord(char))
        if name is None:
            x += size * 0.35
            continue
        pen = SVGPathPen(glyphs)
        glyphs[name].draw(pen)
        commands = pen.getCommands()
        if commands:
            parts.append(
                f'<path d="{commands}" transform="translate({x:.3f} 0) scale({scale:.5f})"/>'
            )
        x += glyphs[name].width * scale + tracking
    return "".join(parts), max(0.0, x - tracking)


def lockup_svg() -> str:
    """SVG lockup used on the startup screen and onboarding."""
    size = 31.0
    tracking = 1.15
    # The wordmark is converted from the bundled Poppins typeface to paths, so
    # it does not depend on fonts installed on the machine rendering the SVG.
    spider, spider_w = glyph_run("SPIDER", FONT_DIR / "Poppins-SemiBold.ttf", size, tracking)
    client, client_w = glyph_run("CLIENT", FONT_DIR / "Poppins-Light.ttf", size, tracking + 0.25)
    baseline = 43.0
    gap = 7.0
    text_x = 70.0
    width = text_x + spider_w + gap + client_w + 2.0
    client_x = text_x + spider_w + gap
    return f'''<svg width="{width:.1f}" height="64" viewBox="0 0 {width:.1f} 64" fill="none" xmlns="http://www.w3.org/2000/svg">
  <g transform="translate(6 8) scale(0.72)">
    {spider_svg(64).split(">", 1)[1].rsplit("</svg", 1)[0]}
  </g>
  <g transform="translate({text_x:.2f} {baseline:.2f}) scale(1 -1)" fill="currentColor">
    {spider}
    <g transform="translate({client_x - text_x:.2f} 0)" fill="currentColor" opacity=".62">{client}</g>
  </g>
</svg>
'''


def draw_spider(draw: ImageDraw.ImageDraw, cx: float, cy: float, scale: float, color, eyes) -> None:
    """Draw the spider on a 64-unit grid, scaled for supersampling."""
    def p(x: float, y: float) -> tuple[float, float]:
        return (cx + (x - 32) * scale, cy + (y - 29) * scale)

    width = max(1, round(2.15 * scale))
    thread_width = max(1, round(1.2 * scale))
    draw.line([p(32, 0.75), p(32, 17.5)], fill=color, width=thread_width)
    curves = [
        ((27.5, 23.2), (23.4, 20.1), (20.1, 16.2), (18.2, 11.7)),
        ((26.8, 27.4), (21.9, 26.7), (17.5, 24.9), (13.6, 22.0)),
        ((26.9, 31.6), (21.4, 33.1), (16.9, 35.6), (13.2, 39.4)),
        ((28.4, 35.6), (24.4, 40.0), (21.8, 45.2), (20.9, 50.8)),
        ((36.5, 23.2), (40.6, 20.1), (43.9, 16.2), (45.8, 11.7)),
        ((37.2, 27.4), (42.1, 26.7), (46.5, 24.9), (50.4, 22.0)),
        ((37.1, 31.6), (42.6, 33.1), (47.1, 35.6), (50.8, 39.4)),
        ((35.6, 35.6), (39.6, 40.0), (42.2, 45.2), (43.1, 50.8)),
    ]
    for start, c1, c2, end in curves:
        points = []
        for i in range(13):
            t = i / 12
            a = ((1 - t) ** 3, 3 * (1 - t) ** 2 * t, 3 * (1 - t) * t * t, t**3)
            x = a[0] * start[0] + a[1] * c1[0] + a[2] * c2[0] + a[3] * end[0]
            y = a[0] * start[1] + a[1] * c1[1] + a[2] * c2[1] + a[3] * end[1]
            points.append(p(x, y))
        draw.line(points, fill=color, width=width, joint="curve")
        r = width / 2
        for x, y in (points[0], points[-1]):
            draw.ellipse((x - r, y - r, x + r, y + r), fill=color)

    # Abdomen / body with a small head notch.
    left, top = p(20.1, 17.6)
    right, bottom = p(43.9, 41.4)
    draw.ellipse((left, top, right, bottom), fill=color)
    head_r = 3.1 * scale
    hx, hy = p(32, 18.2)
    draw.ellipse((hx - head_r, hy - head_r, hx + head_r, hy + head_r), fill=color)

    eye_r = 1.35 * scale
    for ex in (28.6, 35.4):
        x, y = p(ex, 28.8)
        draw.ellipse((x - eye_r, y - eye_r, x + eye_r, y + eye_r), fill=eyes)


def icon_image(size: int) -> Image.Image:
    ss = 4
    canvas = Image.new("RGBA", (size * ss, size * ss), (0, 0, 0, 0))
    d = ImageDraw.Draw(canvas)
    radius = size * ss * 0.22
    d.rounded_rectangle((0, 0, size * ss - 1, size * ss - 1), radius=radius, fill=SURFACE)
    # Hairline edge keeps the icon legible on near-black docks.
    d.rounded_rectangle(
        (2 * ss, 2 * ss, size * ss - 2 * ss, size * ss - 2 * ss),
        radius=radius - 2 * ss,
        outline=BORDER,
        width=max(1, 2 * ss),
    )
    scale = (size * ss / 1024) * 13.0
    draw_spider(d, size * ss / 2, size * ss / 2 - 8 * ss, scale, PAPER, SURFACE)
    return canvas.resize((size, size), Image.Resampling.LANCZOS)


def tray_image(size: int) -> Image.Image:
    ss = 4
    canvas = Image.new("RGBA", (size * ss, size * ss), (0, 0, 0, 0))
    d = ImageDraw.Draw(canvas)
    scale = (size * ss / 64) * 0.86
    draw_spider(d, size * ss / 2, size * ss / 2 - 1 * ss, scale, (255, 255, 255, 255), (0, 0, 0, 0))
    return canvas.resize((size, size), Image.Resampling.LANCZOS)


def write_icns(path: Path, images: list[Image.Image]) -> None:
    """Write the common PNG-backed ICNS chunks without an external tool."""
    chunks = []
    mapping = [
        (b"icp4", images[0]),   # 16
        (b"icp5", images[1]),   # 32
        (b"icp6", images[2]),   # 64
        (b"ic07", images[3]),   # 128
        (b"ic08", images[4]),   # 256
        (b"ic09", images[5]),   # 512
        (b"ic10", images[6]),   # 1024
    ]
    from io import BytesIO

    for tag, image in mapping:
        buf = BytesIO()
        image.save(buf, format="PNG")
        payload = buf.getvalue()
        chunks.append(tag + struct.pack(">I", len(payload) + 8) + payload)
    body = b"".join(chunks)
    path.write_bytes(b"icns" + struct.pack(">I", len(body) + 8) + body)


def main() -> None:
    ASSETS.mkdir(parents=True, exist_ok=True)
    ICONS.mkdir(parents=True, exist_ok=True)

    (ASSETS / "spider-mark.svg").write_text(spider_svg(), encoding="utf-8")
    (ASSETS / "spider-mark-wink.svg").write_text(spider_svg(wink=True), encoding="utf-8")
    (ASSETS / "logo.svg").write_text(lockup_svg(), encoding="utf-8")
    (ASSETS / "icons" / "icon-logo.svg").write_text(spider_svg(80), encoding="utf-8")

    # Legacy names remain so existing packager and window code keep working.
    sizes = {
        "32x32.png": 32,
        "64x64.png": 64,
        "128x128.png": 128,
        "128x128@2x.png": 256,
        "256x256.png": 256,
        "512x512.png": 512,
        "StoreLogo.png": 50,
        "icon.png": 512,
    }
    for name, size in sizes.items():
        icon_image(size).save(ICONS / name)
    tray_image(64).save(ICONS / "tray.png")

    icon_image(1024).save(
        ICONS / "icon.ico",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    write_icns(
        ICONS / "icon.icns",
        [icon_image(n) for n in (16, 32, 64, 128, 256, 512, 1024)],
    )

    print("wrote SpiderClient mark, lockup, and launcher icons")


if __name__ == "__main__":
    main()
