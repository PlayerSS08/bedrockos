#!/usr/bin/env python3
"""Packs the programs in user/bin into user/initfs.bin, which the kernel embeds and installs in /bin.

Format (little endian): "AINI", u16 count, then per program: u8 name length, name, u32 length, u32 packed
length, packed bytes. Packing: every zero byte run becomes 0x00 + u16 run length (ELF files are mostly
page padding, so this alone shrinks them by about two thirds); other bytes are copied. The decoder is
`initfs::unpack`, which has host tests.
"""
import os
import struct
import sys


def rle(data: bytes) -> bytes:
    out = bytearray()
    i = 0
    while i < len(data):
        if data[i] == 0:
            j = i
            while j < len(data) and data[j] == 0 and j - i < 0xFFFF:
                j += 1
            out += b"\x00" + struct.pack("<H", j - i)
            i = j
        else:
            out.append(data[i])
            i += 1
    return bytes(out)


def main() -> None:
    root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "user")
    bin_dir = os.path.join(root, "bin")
    names = sorted(n for n in os.listdir(bin_dir) if os.path.isfile(os.path.join(bin_dir, n)))
    blob = bytearray(b"AINI" + struct.pack("<H", len(names)))
    for n in names:
        data = open(os.path.join(bin_dir, n), "rb").read()
        packed = rle(data)
        blob += bytes([len(n)]) + n.encode() + struct.pack("<II", len(data), len(packed)) + packed
        print(f"{n}: {len(data)} -> {len(packed)} bytes")
    open(os.path.join(root, "initfs.bin"), "wb").write(blob)
    print(f"initfs.bin: {len(blob)} bytes, {len(names)} programs")


if __name__ == "__main__":
    sys.exit(main())
