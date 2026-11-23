#!/usr/bin/env python3
"""VirtualBox boot test for BedrockOS (no QEMU). Standard library only.

For each resolution: (re)attach out/bedrockos.vdi to the VM, start it headless, wait until the serial log
shows the expected markers, take a screenshot with `VBoxManage controlvm screenshotpng`, check its size and
that it is not blank, then power the VM off.

    python tests/vbox_boot.py                       # 1024x768 and 1920x1080
    python tests/vbox_boot.py --res 1024x768 --keep # leave the VM running afterwards

Screenshots land in out/test-shots/. Exit code 0 only when every check passes.
"""
import argparse
import os
import shutil
import struct
import subprocess
import sys
import time
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "out")
SHOTS = os.path.join(OUT, "test-shots")
VM = "BedrockOS"
SERIAL = os.path.join(OUT, "serial.log")


def vboxmanage():
    for c in [shutil.which("VBoxManage"),
              os.path.join(os.environ.get("VBOX_MSI_INSTALL_PATH", ""), "VBoxManage.exe"),
              r"C:\Program Files\Oracle\VirtualBox\VBoxManage.exe",
              "/Applications/VirtualBox.app/Contents/MacOS/VBoxManage"]:
        if c and os.path.isfile(c):
            return c
    sys.exit("VBoxManage not found")


VBM = vboxmanage()


def vb(*args, check=True):
    r = subprocess.run([VBM, *args], capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"VBoxManage {' '.join(args)}: {r.stderr.strip()}")
    return r.stdout


def running():
    return f'"{VM}"' in vb("list", "runningvms")


def poweroff():
    if running():
        vb("controlvm", VM, "poweroff", check=False)
        for _ in range(50):
            if not running():
                break
            time.sleep(0.2)
        time.sleep(1.0)


def ensure_vm(res, net="nat"):
    """Uses the same settings as scripts/vbox.ps1 / vbox.sh (calls the script's create action)."""
    if os.name == "nt":
        cmd = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
               os.path.join(ROOT, "scripts", "vbox.ps1"), "-Action", "create", "-Res", res, "-Net", net]
    else:
        cmd = ["sh", os.path.join(ROOT, "scripts", "vbox.sh"), "create", "--res", res, "--net", net]
    subprocess.run(cmd, check=True, capture_output=True)


def read_serial():
    try:
        with open(SERIAL, "rb") as f:
            return f.read().decode("utf-8", "replace")
    except OSError:
        return ""


def wait_for(markers, timeout):
    t0 = time.time()
    while time.time() - t0 < timeout:
        log = read_serial()
        if all(m in log for m in markers):
            return True, time.time() - t0
        time.sleep(0.5)
    return False, time.time() - t0


def read_png(path):
    """Minimal PNG decoder: 8-bit RGB/RGBA, non-interlaced (what VirtualBox writes). Returns (w, h, rows)."""
    with open(path, "rb") as f:
        data = f.read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat, w = 8, b"", 0
    while pos < len(data):
        ln, typ = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + ln]
        if typ == b"IHDR":
            w, h, depth, ctype, _, _, interlace = struct.unpack(">IIBBBBB", body)
            assert depth == 8 and ctype in (2, 6) and interlace == 0, f"unsupported PNG {depth} {ctype}"
            bpp = 3 if ctype == 2 else 4
        elif typ == b"IDAT":
            idat += body
        pos += 12 + ln
    raw = zlib.decompress(idat)
    stride = w * bpp
    rows, prev = [], bytearray(stride)
    for y in range(h):
        ft = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if ft == 1:
                line[i] = (line[i] + a) & 255
            elif ft == 2:
                line[i] = (line[i] + b) & 255
            elif ft == 3:
                line[i] = (line[i] + (a + b) // 2) & 255
            elif ft == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append([tuple(line[x * bpp:x * bpp + 3]) for x in range(w)])
        prev = line
    return w, h, rows


def screenshot(name):
    os.makedirs(SHOTS, exist_ok=True)
    path = os.path.join(SHOTS, name)
    vb("controlvm", VM, "screenshotpng", path)
    return path


class Result:
    def __init__(self):
        self.rows = []

    def check(self, name, ok, detail=""):
        self.rows.append((name, ok, detail))
        print(f"  [{'PASS' if ok else 'FAIL'}] {name}{(': ' + detail) if detail else ''}", flush=True)
        return ok

    @property
    def ok(self):
        return all(r[1] for r in self.rows)


def boot_at(res, r, keep):
    print(f"\n== boot at {res} ==", flush=True)
    poweroff()
    ensure_vm(res)
    if os.path.exists(SERIAL):
        os.remove(SERIAL)
    vb("startvm", VM, "--type", "headless")
    ok, t = wait_for(["BedrockOS", "boot complete"], 90)
    r.check(f"{res}: serial shows 'boot complete'", ok, f"{t:.1f} s")
    ok2, _ = wait_for(["BedrockOS shell"], 20)
    r.check(f"{res}: shell started", ok2)
    log = read_serial()
    r.check(f"{res}: no panic in log", "PANIC" not in log.upper() and "has stopped" not in log)
    time.sleep(1.5)
    shot = screenshot(f"boot-{res}.png")
    w, h, px = read_png(shot)
    want_w, want_h = (int(v) for v in res.split("x"))
    r.check(f"{res}: framebuffer size", (w, h) == (want_w, want_h), f"got {w}x{h}")
    lit = sum(1 for y in range(0, h, 8) for x in range(0, w, 8) if sum(px[y][x]) > 60)
    total = len(range(0, h, 8)) * len(range(0, w, 8))
    r.check(f"{res}: screen is not blank", lit > total * 0.002, f"{lit}/{total} sampled pixels lit")
    if not keep:
        poweroff()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--res", action="append", help="resolution(s) to test, e.g. 1024x768")
    ap.add_argument("--keep", action="store_true", help="leave the VM running after the last test")
    a = ap.parse_args()
    if not os.path.exists(os.path.join(OUT, "bedrockos.vdi")):
        sys.exit("out/bedrockos.vdi missing: run `cargo xtask build` first")
    r = Result()
    resolutions = a.res or ["1024x768", "1920x1080"]
    for i, res in enumerate(resolutions):
        boot_at(res, r, a.keep and i == len(resolutions) - 1)
    passed = sum(1 for x in r.rows if x[1])
    print(f"\n{passed}/{len(r.rows)} checks passed")
    sys.exit(0 if r.ok else 1)


if __name__ == "__main__":
    main()
