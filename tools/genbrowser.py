#!/usr/bin/env python3
"""Turns assets/browser.png (the Crafting Table icon) into src/gui/browsericon.bin: "BRIC", u16 width,
u16 height (little endian), then width*height RGBA bytes, at 128x128. The desktop scales it down for the dock
and title bar. Needs Pillow (build time only; the .bin is committed)."""
import os
import struct

from PIL import Image

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
im = Image.open(os.path.join(ROOT, "assets", "browser.png")).convert("RGBA").resize((128, 128), Image.LANCZOS)
with open(os.path.join(ROOT, "src", "gui", "browsericon.bin"), "wb") as f:
    f.write(b"BRIC" + struct.pack("<HH", 128, 128) + im.tobytes())
print("wrote src/gui/browsericon.bin")
