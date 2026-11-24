#!/usr/bin/env python3
"""Builds the BedrockOS boot disk: a GPT disk with
  1. an EFI System Partition (FAT32) holding a copy of a directory tree (normally out/usb), and
  2. an empty FAT32 data partition labelled BRDATA (notes, tasks, config live here).

Pure Python, no third-party modules. Adapted from Bedrock's vmbox/build_disks.py, generalised to any tree.

    python tools/mkimage.py <tree-dir> <out.img> [--esp-mb 64] [--data-mb 128]
"""
import argparse
import os
import struct
import sys
import uuid
import zlib

SECTOR = 512
ESP_TYPE = uuid.UUID("C12A7328-F81F-11D2-BA4B-00A0C93EC93B")
DATA_TYPE = uuid.UUID("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7")  # Microsoft basic data


# ------------------------------------------------------------------------------------------------
# FAT32
# ------------------------------------------------------------------------------------------------

def short_name(name, used):
    """8.3 alias for `name`. Returns (11 bytes, needs_lfn)."""
    up = name.upper()
    base, _, ext = up.rpartition(".") if "." in up[1:] else (up, "", "")
    if not base:
        base, ext = up, ""
    ok = lambda s: all(c.isalnum() or c in "$%'-_@~`!(){}^#&" for c in s)
    if len(base) <= 8 and len(ext) <= 3 and ok(base) and ok(ext) and name == up:
        cand = (base.ljust(8) + ext.ljust(3)).encode()
        if cand not in used:
            used.add(cand)
            return cand, False
    clean = lambda s: "".join(c if (c.isalnum() or c in "$%'-_@~`!(){}^#&") else "_" for c in s)
    b, e = clean(base)[:8], clean(ext)[:3]
    for n in range(1, 1000000):
        tail = "~%d" % n
        cand = (b[: 8 - len(tail)] + tail).ljust(8) + e.ljust(3)
        cand = cand.encode()
        if cand not in used:
            used.add(cand)
            return cand, True
    raise RuntimeError("too many similar names")


def lfn_entries(long_name, short11):
    chk = 0
    for c in short11:
        chk = (((chk & 1) << 7) + (chk >> 1) + c) & 0xFF
    units = list(long_name.encode("utf-16-le"))
    units = [units[i] | (units[i + 1] << 8) for i in range(0, len(units), 2)] + [0]
    while len(units) % 13:
        units.append(0xFFFF)
    parts = [units[i:i + 13] for i in range(0, len(units), 13)]
    out = []
    for n in range(len(parts), 0, -1):
        u = parts[n - 1]
        seq = n | (0x40 if n == len(parts) else 0)
        e = (bytes([seq]) + struct.pack("<5H", *u[0:5]) + bytes([0x0F, 0, chk]) +
             struct.pack("<6H", *u[5:11]) + b"\0\0" + struct.pack("<2H", *u[11:13]))
        out.append(e)
    return out


def dirent(name11, attr, cluster, size):
    e = bytearray(32)
    e[0:11] = name11
    e[11] = attr
    # 2026-01-01 00:00 for created/written dates
    date = ((2026 - 1980) << 9) | (1 << 5) | 1
    struct.pack_into("<HH", e, 14, 0, date)
    struct.pack_into("<H", e, 18, date)
    struct.pack_into("<H", e, 20, (cluster >> 16) & 0xFFFF)
    struct.pack_into("<HH", e, 22, 0, date)
    struct.pack_into("<H", e, 26, cluster & 0xFFFF)
    struct.pack_into("<I", e, 28, size)
    return bytes(e)


class Fat32:
    def __init__(self, total_sectors, label):
        self.spc = 1
        self.reserved = 32
        fat = 1
        while True:
            clusters = (total_sectors - self.reserved - 2 * fat) // self.spc
            if ((clusters + 2) * 4 + SECTOR - 1) // SECTOR <= fat:
                break
            fat += 1
        if clusters < 65525:
            raise SystemExit(f"partition too small for FAT32 ({clusters} clusters); use at least 34 MB")
        self.fat_sectors = fat
        self.clusters = clusters
        self.total = total_sectors
        self.label = label.upper().ljust(11)[:11].encode()
        self.img = bytearray(total_sectors * SECTOR)
        self.next = 3  # cluster 2 is the root directory
        self.first_data = self.reserved + 2 * fat
        self.fat_set(0, 0x0FFFFFF8)
        self.fat_set(1, 0x0FFFFFFF)
        self.fat_set(2, 0x0FFFFFFF)

    def fat_set(self, c, v):
        for k in range(2):
            o = (self.reserved + k * self.fat_sectors) * SECTOR + c * 4
            struct.pack_into("<I", self.img, o, v)

    def coff(self, c):
        return (self.first_data + (c - 2) * self.spc) * SECTOR

    def alloc(self, data, minimum=0):
        csz = self.spc * SECTOR
        n = max((len(data) + csz - 1) // csz, minimum)
        if n == 0:
            return 0
        first = self.next
        if first + n - 2 > self.clusters:
            raise SystemExit("image full")
        self.next += n
        for i in range(n):
            c = first + i
            self.fat_set(c, c + 1 if i + 1 < n else 0x0FFFFFFF)
            chunk = data[i * csz:(i + 1) * csz]
            self.img[self.coff(c):self.coff(c) + len(chunk)] = chunk
        return first

    def write_dir_at(self, cluster, data):
        """Writes directory data to an already-allocated chain starting at `cluster` (contiguous)."""
        self.img[self.coff(cluster):self.coff(cluster) + len(data)] = data

    def add_tree(self, path, cluster, parent):
        """Copies host directory `path` into the directory whose first cluster is `cluster`."""
        entries = bytearray()
        used = set()
        if cluster == 2:
            entries += dirent(self.label, 0x08, 0, 0)
        else:
            entries += dirent(b".          ", 0x10, cluster, 0)
            entries += dirent(b"..         ", 0x10, 0 if parent == 2 else parent, 0)
        subdirs = []
        for name in sorted(os.listdir(path)):
            full = os.path.join(path, name)
            short, lfn = short_name(name, used)
            if lfn:
                for e in lfn_entries(name, short):
                    entries += e
            if os.path.isdir(full):
                # Reserve the directory's clusters now; filled after we know its size.
                n_entries = 2 + sum(1 + (len(x) + 12) // 13 + 1 for x in os.listdir(full))
                need = max(1, (n_entries * 32 + SECTOR * self.spc - 1) // (SECTOR * self.spc))
                c = self.alloc(b"", minimum=need)
                entries += dirent(short, 0x10, c, 0)
                subdirs.append((full, c))
            else:
                with open(full, "rb") as f:
                    data = f.read()
                c = self.alloc(data)
                entries += dirent(short, 0x20, c, len(data))
        if cluster == 2:
            # Root may need more than one cluster: allocate extra and chain them after cluster 2.
            csz = SECTOR * self.spc
            if len(entries) > csz:
                extra = self.alloc(b"", minimum=(len(entries) - 1) // csz)
                self.fat_set(2, extra)
                self.img[self.coff(2):self.coff(2) + csz] = entries[:csz]
                self.img[self.coff(extra):self.coff(extra) + len(entries) - csz] = entries[csz:]
            else:
                self.write_dir_at(2, entries)
        else:
            self.write_dir_at(cluster, entries)
        for full, c in subdirs:
            self.add_tree(full, c, cluster)

    def finish(self, volume_id):
        bs = bytearray(SECTOR)
        bs[0:3] = b"\xEB\x58\x90"
        bs[3:11] = b"BEDROCK "
        struct.pack_into("<HBHBHHBHHHII", bs, 11, SECTOR, self.spc, self.reserved, 2, 0, 0, 0xF8, 0, 32, 64,
                         0, self.total)
        struct.pack_into("<IHHIHH", bs, 36, self.fat_sectors, 0, 0, 2, 1, 6)
        bs[64] = 0x80
        bs[66] = 0x29
        struct.pack_into("<I", bs, 67, volume_id)
        bs[71:82] = self.label
        bs[82:90] = b"FAT32   "
        bs[510:512] = b"\x55\xAA"
        fsinfo = bytearray(SECTOR)
        fsinfo[0:4] = b"RRaA"
        fsinfo[484:488] = b"rrAa"
        struct.pack_into("<II", fsinfo, 488, self.clusters - (self.next - 2), self.next)
        fsinfo[508:512] = b"\x00\x00\x55\xAA"
        for base in (0, 6):
            self.img[base * SECTOR:(base + 1) * SECTOR] = bs
            self.img[(base + 1) * SECTOR:(base + 2) * SECTOR] = fsinfo
        return self.img


# ------------------------------------------------------------------------------------------------
# GPT
# ------------------------------------------------------------------------------------------------

def gpt_entry(type_guid, start, end, name):
    e = bytearray(128)
    e[0:16] = type_guid.bytes_le
    e[16:32] = uuid.uuid4().bytes_le
    struct.pack_into("<QQQ", e, 32, start, end, 0)
    n = name.encode("utf-16-le")[:72]
    e[56:56 + len(n)] = n
    return e


def gpt_header(current, backup, first_usable, last_usable, disk_guid, entries_lba, entries_crc):
    h = bytearray(SECTOR)
    struct.pack_into("<8sIII", h, 0, b"EFI PART", 0x00010000, 92, 0)
    struct.pack_into("<QQQQ", h, 24, current, backup, first_usable, last_usable)
    h[56:72] = disk_guid
    struct.pack_into("<QIII", h, 72, entries_lba, 128, 128, entries_crc)
    struct.pack_into("<I", h, 16, zlib.crc32(bytes(h[0:92])) & 0xFFFFFFFF)
    return h


def build(tree, out, esp_mb, data_mb):
    mb = 2048  # sectors per MiB
    esp_start = 2048
    esp_len = esp_mb * mb
    data_start = esp_start + esp_len
    data_len = data_mb * mb
    total = data_start + data_len + 2048  # room for the backup GPT
    # Labelled BEDROCK: the OS saves your files (notes, tasks, ...) only on a FAT32 volume with this label.
    esp = Fat32(esp_len, "BEDROCK")
    esp.add_tree(tree, 2, 2)
    esp_img = esp.finish(0x20261123)
    data = Fat32(data_len, "BRDATA")
    data_img = data.finish(0x20261124)

    disk = bytearray(total * SECTOR)
    mbr = bytearray(SECTOR)
    mbr[446:462] = struct.pack("<B3sB3sII", 0, b"\x00\x02\x00", 0xEE, b"\xFF\xFF\xFF", 1,
                               min(total - 1, 0xFFFFFFFF))
    mbr[510:512] = b"\x55\xAA"
    disk[0:SECTOR] = mbr

    entries = bytearray(128 * 128)
    entries[0:128] = gpt_entry(ESP_TYPE, esp_start, esp_start + esp_len - 1, "EFI System")
    entries[128:256] = gpt_entry(DATA_TYPE, data_start, data_start + data_len - 1, "BedrockOS data")
    crc = zlib.crc32(bytes(entries)) & 0xFFFFFFFF
    disk_guid = uuid.uuid4().bytes_le
    last = total - 34
    first_usable = 34
    disk[SECTOR:2 * SECTOR] = gpt_header(1, total - 1, first_usable, last - 1, disk_guid, 2, crc)
    disk[2 * SECTOR:34 * SECTOR] = entries
    disk[(total - 33) * SECTOR:(total - 1) * SECTOR] = entries
    disk[(total - 1) * SECTOR:total * SECTOR] = gpt_header(total - 1, 1, first_usable, last - 1, disk_guid,
                                                           total - 33, crc)
    disk[esp_start * SECTOR:(esp_start + esp_len) * SECTOR] = esp_img
    disk[data_start * SECTOR:(data_start + data_len) * SECTOR] = data_img
    with open(out, "wb") as f:
        f.write(disk)
    print(f"wrote {out}: {len(disk) // (1024 * 1024)} MiB (ESP {esp_mb} MiB, data {data_mb} MiB)")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("tree")
    ap.add_argument("out")
    ap.add_argument("--esp-mb", type=int, default=64)
    ap.add_argument("--data-mb", type=int, default=128)
    a = ap.parse_args()
    if not os.path.isdir(a.tree):
        sys.exit(f"not a directory: {a.tree}")
    build(a.tree, a.out, a.esp_mb, a.data_mb)


if __name__ == "__main__":
    main()
