#!/usr/bin/env python3
"""Rasterize the agent marks the Mac app shows into the firmware's alpha masks.

The SVGs are the app's own (host/crates/openwalkietalkie-app/assets/icons),
so the deck's roster and the desktop show the same marks. With `--all` each
becomes a 24 px (assets/marks/<name>.alpha) and a 40 px
(assets/marks/<name>-40.alpha) 8-bit alpha mask, rendered at 8x by
ImageMagick and downsampled with Pillow. `other` reuses Lucide's bot glyph.

Conol is drawn from its brand mascot's own geometry (conol's
apps/conol/components/mascot.tsx, traced from its 1024 px logo), not from the
app's 16x16 mosaic, which no deck size samples cleanly: the strokes, circles
and Bezier head are painted at 16x and box-filtered, so every edge pixel gets
its true coverage. Requires ImageMagick (`magick`) and Pillow.
"""
import re
import subprocess
import tempfile
from pathlib import Path
from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
ICONS = ROOT / "host/crates/openwalkietalkie-app/assets/icons"
OUT = ROOT / "assets/marks"
SIZE, SCALE = 24, 8
MARKS = {"claude": "claude-mark", "codex": "openai-mark", "gemini": "gemini-mark", "conol": "conol-mark"}


# Conol's mascot (viewBox 0 0 200 200, plate off): ink strokes 9.3 wide.
MASCOT_STROKE, SMILE_STROKE = 9.3, 8.3
MASCOT_ANTENNAS = [((100, 36), (100, 84)), ((69.55, 51.05), (82.85, 76.15)), ((130.45, 51.05), (117.15, 76.15))]
MASCOT_BALLS = [(100, 35.9, 10.35), (69.55, 51.05, 10.6), (130.45, 51.05, 10.6)]
MASCOT_EYES = [(76.95, 113.25, 8.95), (123.05, 113.25, 8.95)]
MASCOT_HEAD = [  # cubic segments of the dome-topped squircle, from (100, 70.7)
    ((116.45, 70.7), (130.1, 75.5), (140.05, 84)), ((146.45, 89.45), (155.4, 99.95), (155.4, 117.1)),
    ((155.4, 138.95), (152.3, 143.25), (150.4, 146.6)), ((147.25, 152.15), (142.6, 156.2), (135.6, 159.1)),
    ((132.65, 160.35), (125.5, 163.55), (100, 163.55)), ((74.5, 163.55), (67.35, 160.35), (64.4, 159.1)),
    ((57.4, 156.2), (52.75, 152.15), (49.6, 146.6)), ((47.7, 143.25), (44.6, 138.95), (44.6, 117.1)),
    ((44.6, 99.95), (53.55, 89.45), (59.95, 84)), ((69.9, 75.5), (83.55, 70.7), (100, 70.7)),
]
MASCOT_SMILE = ((83.25, 136.45), (100, 150.6), (116.75, 136.45))
# The top of the top antenna ball, where the ink starts.
MASCOT_TOP = 35.9 - 10.35


def mascot(size: int, ss: int = 16) -> Image.Image:
    """Conol's mascot as an alpha mask `size` px tall, centred. Below 32 px
    the strokes and eyes are drawn heavier (about 2 px instead of 1.5), as
    small icon sizes are, so the 24 px roster mark keeps its weight next to
    the solid Claude and Codex marks."""
    weight, eye_weight = (1.3, 1.15) if size < 32 else (1.0, 1.0)
    head_w, smile_w = MASCOT_STROKE * weight, SMILE_STROKE * weight
    bottom = 163.55 + head_w / 2
    big = size * ss
    scale = (size - 0.5) / (bottom - MASCOT_TOP) * ss
    def at(x, y):
        return (big / 2 + (x - 100) * scale, 0.25 * ss + (y - MASCOT_TOP) * scale)
    image = Image.new("L", (big, big))
    draw = ImageDraw.Draw(image)
    def disc(cx, cy, r, fill=255):
        x, y = at(cx, cy)
        draw.ellipse([x - r * scale, y - r * scale, x + r * scale, y + r * scale], fill=fill)
    def stroke(points, width):
        draw.line([at(*p) for p in points], fill=255, width=round(width * scale), joint="curve")
        for p in (points[0], points[-1]):
            disc(*p, width / 2)
    def bezier(p0, controls, steps=48):
        points = []
        for t in (i / steps for i in range(1, steps + 1)):
            if len(controls) == 3:
                (x1, y1), (x2, y2), (x3, y3) = controls
                a, b, c, d = (1 - t) ** 3, 3 * (1 - t) ** 2 * t, 3 * (1 - t) * t * t, t ** 3
                points.append((a * p0[0] + b * x1 + c * x2 + d * x3, a * p0[1] + b * y1 + c * y2 + d * y3))
            else:
                (x1, y1), (x2, y2) = controls
                a, b, c = (1 - t) ** 2, 2 * (1 - t) * t, t * t
                points.append((a * p0[0] + b * x1 + c * x2, a * p0[1] + b * y1 + c * y2))
        return points
    # Antennas, then the head's face erases their inner ends (the logo's
    # yellow fill), then the outline, eyes and smile on top.
    for line in MASCOT_ANTENNAS:
        stroke(line, head_w)
    for ball in MASCOT_BALLS:
        disc(*ball)
    head, start = [(100, 70.7)], (100, 70.7)
    for segment in MASCOT_HEAD:
        head += bezier(start, segment)
        start = segment[-1]
    draw.polygon([at(*p) for p in head], fill=0)
    stroke(head, head_w)
    for cx, cy, r in MASCOT_EYES:
        disc(cx, cy, r * eye_weight)
    stroke([MASCOT_SMILE[0]] + bezier(MASCOT_SMILE[0], MASCOT_SMILE[1:]), smile_w)
    return image.resize((size, size), Image.Resampling.BOX)


def render(svg: Path) -> Image.Image:
    source = svg.read_text().replace("currentColor", "#ffffff")
    with tempfile.TemporaryDirectory() as tmp:
        src, png = Path(tmp) / "mark.svg", Path(tmp) / "mark.png"
        src.write_text(source)
        big = SIZE * SCALE
        subprocess.run(["magick", "-background", "none", "-density", "1200", str(src),
                        "-resize", f"{big}x{big}", "-gravity", "center", "-extent", f"{big}x{big}", str(png)],
                       check=True)
        image = Image.open(png).convert("RGBA")
    # Alpha only: the firmware tints the mask with the mark's colour.
    return image.getchannel("A").resize((SIZE, SIZE), Image.Resampling.LANCZOS)


def main():
    """`--size N --out DIR` renders another size elsewhere (layout studies)."""
    import argparse
    global SIZE
    parser = argparse.ArgumentParser()
    parser.add_argument("--size", type=int, default=SIZE)
    parser.add_argument("--out", type=Path, default=OUT)
    parser.add_argument("--all", action="store_true",
                        help="the firmware's set: 24 px as <name>.alpha, 40 px as <name>-40.alpha")
    args = parser.parse_args()
    runs = [(24, ""), (40, "-40")] if args.all else [(args.size, "")]
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    for size, suffix in runs:
        SIZE = size
        for name, icon in MARKS.items():
            image = mascot(SIZE) if name == "conol" else render(ICONS / f"{icon}.svg")
            (out / f"{name}{suffix}.alpha").write_bytes(image.tobytes())
        bot = Image.frombytes("L", (40, 40), (ROOT / "assets/lucide/bot.alpha").read_bytes())
        (out / f"other{suffix}.alpha").write_bytes(bot.resize((SIZE, SIZE), Image.Resampling.LANCZOS).tobytes())
        print(f"Generated {len(MARKS) + 1} agent marks ({SIZE}x{SIZE} alpha) in {out}")


if __name__ == "__main__":
    main()
