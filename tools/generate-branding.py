#!/usr/bin/env python3
"""Derive app, menu-bar and web branding from the existing website radio.

Requires Pillow. Checked-in outputs are used by normal builds. The radio's
pixel grid stays intact; the desktop app icons share the rounded background.
"""
from pathlib import Path
import xml.etree.ElementTree as ET

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "assets/branding"


def main():
    OUT.mkdir(exist_ok=True)
    radio = Image.open(ROOT / "site/public/assets/logo-64.png").convert("RGBA")
    grid = radio.resize((16, 16), Image.Resampling.NEAREST)
    radio = grid.crop(grid.getbbox())
    radio.resize((96, 112), Image.Resampling.NEAREST).save(OUT / "radio.png")

    app = Image.new("RGBA", (1024, 1024))
    draw = ImageDraw.Draw(app)
    draw.rounded_rectangle((60, 60, 964, 964), radius=180, fill="#fffaec")
    app.alpha_composite(radio.resize((480, 560), Image.Resampling.NEAREST), (272, 232))
    app.save(OUT / "app-icon.png")
    app.save(OUT / "OpenWalkieTalkie.icns")
    app.save(OUT / "OpenWalkieTalkie.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])

    # A monochrome version with the same antenna, screen and three keys.
    mask = Image.new("L", (36, 36))
    draw = ImageDraw.Draw(mask)
    draw.rectangle((6, 8, 29, 31), fill=255)
    draw.rectangle((24, 2, 25, 9), fill=255)
    draw.rectangle((10, 12, 25, 21), fill=0)
    draw.rectangle((12, 14, 15, 15), fill=255)
    draw.rectangle((12, 18, 19, 19), fill=255)
    for x in (10, 16, 22):
        draw.rectangle((x, 24, x + 3, 27), fill=0)
    (OUT / "radio-template.alpha").write_bytes(mask.tobytes())

    # Reuse the supplied symbol paths, with a tight viewBox and theme tint.
    source = ET.parse(ROOT / "assets/CoHDL-透明白2k.svg").getroot()
    symbol = source[0]
    for node in symbol.iter():
        if node.get("fill") == "#FFFFFF":
            node.set("fill", "currentColor")
        if node.get("stroke") == "#FFFFFF":
            node.set("stroke", "currentColor")
    ET.register_namespace("", "http://www.w3.org/2000/svg")
    mark = ET.Element("{http://www.w3.org/2000/svg}svg", {"viewBox": "345 565 635 785"})
    mark.append(symbol)
    svg = ET.tostring(mark, encoding="unicode")
    (OUT / "cohdl-mark.svg").write_text(svg)
    (ROOT / "host/crates/openwalkietalkie-app/assets/icons/cohdl-mark.svg").write_text(svg)
    # A standalone img cannot inherit the page's currentColor.
    (ROOT / "site/public/assets/cohdl-mark.svg").write_text(svg.replace("currentColor", "#8a6a4c"))
    print("Generated app icon, radio, template mask and COHDL mark")


if __name__ == "__main__":
    main()
