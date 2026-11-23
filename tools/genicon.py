#!/usr/bin/env python3
"""Turns the ChatBox window icon image into src/gui/icondata.bin.

Run from the repo root:  python tools/genicon.py [image]
The image defaults to assets/bedrock.png or assets/bedrock.jpg (any size; a centred square is cut out of it).
Images without transparency (JPG) get their light background removed: light pixels connected to the
border (plain white or a baked-in "transparent" checkerboard) become transparent.
Needs Pillow (build time only; the generated file is committed, nothing is needed to build BedrockOS).
Without an image the output is empty and the desktop draws a placeholder icon instead.

Format: for each size, "BRIC", u16 width, u16 height (little endian), then width*height RGBA bytes.
"""
import io
import os
import struct
import sys

from PIL import Image

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "src", "gui", "icondata.bin")
# Title-bar icon sizes: 20 px for screens under 1600 wide, 28 px above (title bar height minus 12).
SIZES = [20, 28]


def remove_background(im, light=185):
    """Makes light, low-saturation pixels reachable from the border transparent (flood fill)."""
    w, h = im.size
    px = im.load()

    def is_bg(p):
        r, g, b, _ = p
        return min(r, g, b) >= light and max(r, g, b) - min(r, g, b) <= 24

    seen = bytearray(w * h)
    stack = [(x, y) for x in range(w) for y in (0, h - 1)] + [(x, y) for y in range(h) for x in (0, w - 1)]
    while stack:
        x, y = stack.pop()
        if x < 0 or y < 0 or x >= w or y >= h or seen[y * w + x]:
            continue
        seen[y * w + x] = 1
        if not is_bg(px[x, y]):
            continue
        px[x, y] = (0, 0, 0, 0)
        stack += [(x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)]
    # Crop to what is left, so the subject fills the icon.
    box = im.getbbox()
    return im.crop(box) if box else im


def main():
    if len(sys.argv) > 1:
        src = sys.argv[1]
    else:
        cands = [os.path.join(ROOT, "assets", "bedrock." + e) for e in ("png", "jpg", "jpeg")]
        src = next((c for c in cands if os.path.exists(c)), cands[0])
    if not os.path.exists(src):
        io.open(OUT, "wb").close()
        print(f"{src} not found: wrote an empty {OUT} (placeholder icon)")
        return
    im = Image.open(src)
    has_alpha = im.mode in ("RGBA", "LA") or "transparency" in im.info
    im = im.convert("RGBA")
    if not has_alpha:
        im = remove_background(im)
        # Pad (not crop) to a square, so the whole subject stays visible.
        side = max(im.size)
        sq = Image.new("RGBA", (side, side), (0, 0, 0, 0))
        sq.paste(im, ((side - im.width) // 2, (side - im.height) // 2))
        im = sq
    side = min(im.size)
    left, top = (im.width - side) // 2, (im.height - side) // 2
    im = im.crop((left, top, left + side, top + side))
    out = bytearray()
    for s in SIZES:
        small = im.resize((s, s), Image.LANCZOS)
        out += b"BRIC" + struct.pack("<HH", s, s) + small.tobytes()
    with io.open(OUT, "wb") as f:
        f.write(out)
    print(f"wrote {OUT}: sizes {SIZES} from {src} ({len(out)} bytes)")


if __name__ == "__main__":
    main()
