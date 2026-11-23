#!/usr/bin/env python3
"""Turns the background pictures into QOI files the kernel embeds (src/gui/wallimg.rs decodes them).

Run from the repo root:  python tools/genbg.py
  assets/mc-bg.png    -> assets/mc-bg.qoi     boot screen and boot menu
  assets/mc-bg-hs.png -> assets/mc-bg-hs.qoi  desktop
Needs Pillow (build time only; the .qoi files are committed, nothing is needed to build BedrockOS).
QOI ("Quite OK Image" format, qoiformat.org) is lossless and simple enough to decode in a few lines without
an allocator. Pictures may have at most 1 << 20 pixels (the kernel's decode buffer); larger ones are scaled.
"""
import os
import struct

from PIL import Image

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, "assets")
PAIRS = [("mc-bg.png", "mc-bg.qoi"), ("mc-bg-hs.png", "mc-bg-hs.qoi")]
MAX_PIXELS = 1 << 20


def qoi_encode(im):
    w, h = im.size
    px = im.tobytes()
    out = bytearray(b"qoif" + struct.pack(">IIBB", w, h, 3, 0))
    index = [(0, 0, 0, 0)] * 64
    prev = (0, 0, 0, 255)
    run = 0
    n = w * h
    for i in range(n):
        r, g, b = px[3 * i], px[3 * i + 1], px[3 * i + 2]
        cur = (r, g, b, 255)
        if cur == prev:
            run += 1
            if run == 62 or i == n - 1:
                out.append(0xC0 | (run - 1))
                run = 0
            continue
        if run:
            out.append(0xC0 | (run - 1))
            run = 0
        h_ = (r * 3 + g * 5 + b * 7 + 255 * 11) % 64
        if index[h_] == cur:
            out.append(h_)
        else:
            index[h_] = cur
            dr = (r - prev[0] + 128) % 256 - 128
            dg = (g - prev[1] + 128) % 256 - 128
            db = (b - prev[2] + 128) % 256 - 128
            if -2 <= dr <= 1 and -2 <= dg <= 1 and -2 <= db <= 1:
                out.append(0x40 | (dr + 2) << 4 | (dg + 2) << 2 | (db + 2))
            else:
                dr_dg, db_dg = dr - dg, db - dg
                if -32 <= dg <= 31 and -8 <= dr_dg <= 7 and -8 <= db_dg <= 7:
                    out.append(0x80 | (dg + 32))
                    out.append((dr_dg + 8) << 4 | (db_dg + 8))
                else:
                    out += bytes((0xFE, r, g, b))
        prev = cur
    out += b"\x00" * 7 + b"\x01"
    return bytes(out)


def main():
    for src, dst in PAIRS:
        im = Image.open(os.path.join(ASSETS, src)).convert("RGB")
        w, h = im.size
        if w * h > MAX_PIXELS:
            k = (MAX_PIXELS / (w * h)) ** 0.5
            im = im.resize((int(w * k), int(h * k)), Image.LANCZOS)
        data = qoi_encode(im)
        with open(os.path.join(ASSETS, dst), "wb") as f:
            f.write(data)
        print(f"{src}: {im.size[0]}x{im.size[1]} -> {dst} ({len(data)} bytes)")


if __name__ == "__main__":
    main()
