#!/usr/bin/env python3
"""Builds a bootable BedrockOS ISO (CD/DVD image) for UEFI PCs and virtual machines:

  * an ISO 9660 file system with README.TXT and the EFI boot image (EFIBOOT.IMG), and
  * an El Torito boot catalog with one UEFI entry (platform 0xEF) pointing at EFIBOOT.IMG, a FAT32
    image of the boot tree (normally out/usb: EFI/BOOT/BOOTX64.EFI and bedrock.cfg).

UEFI firmware finds the El Torito entry, mounts EFIBOOT.IMG as a FAT partition and starts
\\EFI\\BOOT\\BOOTX64.EFI. A CD is read-only: notes and tasks are not kept between boots (use the .img
disk for that). Pure Python, no third-party modules.

    python tools/mkiso.py <tree-dir> <out.iso> [--esp-mb 40]
"""
import argparse
import datetime
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mkimage import Fat32  # noqa: E402

BLOCK = 2048
README = b"""BedrockOS - a UEFI operating system written from scratch in Rust.

Boot this ISO on a UEFI PC or virtual machine (VirtualBox: enable EFI, attach the ISO
as an optical drive, 64-bit "Other" OS, 1 GB RAM).

Sign in:  user name BedrockOS
          password  MinecraftSS_08

The CD is read-only, so notes and tasks are not kept between boots.
"""


def both16(v):
    return struct.pack("<H", v) + struct.pack(">H", v)


def both32(v):
    return struct.pack("<I", v) + struct.pack(">I", v)


def dir_date(t):
    return bytes([t.year - 1900, t.month, t.day, t.hour, t.minute, t.second, 0])


def vol_date(t):
    return t.strftime("%Y%m%d%H%M%S00").encode() + b"\x00"


def dirrec(name, lba, size, is_dir, t):
    n = len(name)
    ln = 33 + n + (1 - n % 2)
    r = bytearray(ln)
    r[0] = ln
    r[2:10] = both32(lba)
    r[10:18] = both32(size)
    r[18:25] = dir_date(t)
    r[25] = 2 if is_dir else 0
    r[28:32] = both16(1)
    r[32] = n
    r[33:33 + n] = name
    return bytes(r)


def pad(s, n):
    return s.ljust(n)[:n].encode()


def build(tree, out, esp_mb):
    t = datetime.datetime(2026, 11, 24, 3, 0, 0)
    # The EFI boot image: FAT32 with the boot tree.
    sectors = esp_mb * 2048
    fat = Fat32(sectors, "BEDROCKOS")
    fat.add_tree(tree, 2, 2)
    efi = bytes(fat.finish(0x20261124))

    # Layout (2048-byte blocks): 16 system | 16 PVD | 17 boot record | 18 terminator | 19 L path |
    # 20 M path | 21 root dir | 22 boot catalog | 23 README | 24.. EFIBOOT.IMG
    root_lba, cat_lba, readme_lba, efi_lba = 21, 22, 23, 24
    efi_blocks = (len(efi) + BLOCK - 1) // BLOCK
    total = efi_lba + efi_blocks
    iso = bytearray(total * BLOCK)

    def put(lba, data):
        iso[lba * BLOCK:lba * BLOCK + len(data)] = data

    files = [(b"BOOT.CAT;1", cat_lba, BLOCK), (b"EFIBOOT.IMG;1", efi_lba, len(efi)), (b"README.TXT;1", readme_lba, len(README))]
    root = dirrec(b"\x00", root_lba, BLOCK, True, t) + dirrec(b"\x01", root_lba, BLOCK, True, t)
    for name, lba, size in sorted(files):
        root += dirrec(name, lba, size, False, t)
    assert len(root) <= BLOCK
    put(root_lba, root)

    # Path tables: just the root.
    put(19, struct.pack("<BBIH", 1, 0, root_lba, 1) + b"\x00\x00")
    put(20, struct.pack(">BBIH", 1, 0, root_lba, 1) + b"\x00\x00")

    pvd = bytearray(BLOCK)
    pvd[0] = 1
    pvd[1:6] = b"CD001"
    pvd[6] = 1
    pvd[8:40] = pad("", 32)
    pvd[40:72] = pad("BEDROCKOS", 32)
    pvd[80:88] = both32(total)
    pvd[120:124] = both16(1)
    pvd[124:128] = both16(1)
    pvd[128:132] = both16(BLOCK)
    pvd[132:140] = both32(10)
    struct.pack_into("<I", pvd, 140, 19)
    struct.pack_into(">I", pvd, 148, 20)
    pvd[156:190] = dirrec(b"\x00", root_lba, BLOCK, True, t)
    pvd[190:318] = pad("BEDROCKOS", 128)
    pvd[318:446] = pad("BEDROCKOS", 128)
    pvd[446:574] = pad("BEDROCKOS TOOLS/MKISO.PY", 128)
    pvd[574:702] = pad("BEDROCKOS", 128)
    pvd[702:739] = pad("", 37)
    pvd[739:776] = pad("", 37)
    pvd[776:813] = pad("", 37)
    pvd[813:830] = vol_date(t)
    pvd[830:847] = vol_date(t)
    pvd[847:864] = b"0" * 16 + b"\x00"
    pvd[864:881] = vol_date(t)
    pvd[881] = 1
    put(16, pvd)

    br = bytearray(BLOCK)
    br[0] = 0
    br[1:6] = b"CD001"
    br[6] = 1
    br[7:39] = b"EL TORITO SPECIFICATION".ljust(32, b"\x00")
    struct.pack_into("<I", br, 0x47, cat_lba)
    put(17, br)

    term = bytearray(BLOCK)
    term[0] = 255
    term[1:6] = b"CD001"
    term[6] = 1
    put(18, term)

    # Boot catalog: validation entry (platform 0xEF = UEFI) and the default entry.
    val = bytearray(32)
    val[0] = 1
    val[1] = 0xEF
    val[4:28] = pad("BEDROCKOS", 24)
    val[30] = 0x55
    val[31] = 0xAA
    s = sum(struct.unpack("<16H", bytes(val))) & 0xFFFF
    struct.pack_into("<H", val, 28, (0x10000 - s) & 0xFFFF)
    assert sum(struct.unpack("<16H", bytes(val))) & 0xFFFF == 0
    ent = bytearray(32)
    ent[0] = 0x88  # bootable
    ent[1] = 0  # no emulation
    # Sector count in 512-byte units; 0 when it does not fit 16 bits (firmware then uses the image).
    count = len(efi) // 512
    struct.pack_into("<H", ent, 6, count if count <= 0xFFFF else 0)
    struct.pack_into("<I", ent, 8, efi_lba)
    put(cat_lba, bytes(val) + bytes(ent))

    put(readme_lba, README)
    put(efi_lba, efi)
    with open(out, "wb") as f:
        f.write(iso)
    print(f"wrote {out}: {len(iso) / (1024 * 1024):.1f} MiB (EFI boot image {esp_mb} MiB)")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("tree")
    ap.add_argument("out")
    ap.add_argument("--esp-mb", type=int, default=40)
    a = ap.parse_args()
    if not os.path.isdir(a.tree):
        sys.exit(f"not a directory: {a.tree}")
    build(a.tree, a.out, a.esp_mb)


if __name__ == "__main__":
    main()
