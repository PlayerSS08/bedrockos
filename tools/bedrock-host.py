#!/usr/bin/env python3
"""The laptop-side helper for BedrockOS running in VirtualBox (Windows). Run it while the VM runs:

    python tools/bedrock-host.py

It does two jobs BedrockOS cannot do inside a virtual machine:

1. Wi-Fi through the laptop (src/wifihost.rs). A VM has no Wi-Fi radio, only a virtual network card, so
   `wifi scan` and `wifi connect` in BedrockOS ask this helper over the VM's COM2 port (a named pipe,
   \\\\.\\pipe\\bedrockos-wifi, set up by scripts/vbox.ps1). It answers with the networks the laptop's Wi-Fi sees
   (`netsh wlan show networks`) and makes the LAPTOP join the network chosen in BedrockOS (`netsh wlan connect`,
   adding a WPA2 profile with the password first when the laptop does not know the network). BedrockOS then
   reaches the internet through it. Protocol, one line each, fields separated by tabs:
       HELLO                      -> AEONWIFI 1
       SCAN                       -> NET signal secure current ssid ... then END
       CONNECT ssid password      -> OK text | ERR text

2. Web pages for the Crafting Table browser (src/browser.rs). BedrockOS has no TLS, so it cannot open https://
   pages itself. The browser asks http://10.0.2.2:8086/fetch?u=<address> (10.0.2.2 is the laptop as seen from
   VirtualBox's NAT network); this helper fetches the page (http or https, following redirects), removes scripts
   and styles, and returns the HTML as UTF-8 with the final address in an X-Final-Url header.
"""
import html
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PIPE = r"\\.\pipe\bedrockos-wifi"
WEB_PORT = 8086
MAX_PAGE = 1_500_000
UA = "Mozilla/5.0 (compatible; CraftingTable/1.0; BedrockOS)"


# ---- Wi-Fi ---------------------------------------------------------------------------------------------------

def netsh(*args):
    r = subprocess.run(["netsh", "wlan", *args], capture_output=True)
    out = r.stdout.decode("utf-8", "replace") + r.stderr.decode("utf-8", "replace")
    return out


def netsh_ok(*args):
    return subprocess.run(["netsh", "wlan", *args], capture_output=True).returncode == 0


def current_ssid():
    """The network the laptop is connected to now ("" when none)."""
    text = netsh("show", "interfaces")
    if not re.search(r"^\s+State\s+:\s+connected", text, re.M):
        return ""
    m = re.search(r"^\s+SSID\s+:\s(.*)$", text, re.M)
    return m.group(1).strip() if m else ""


def fresh_scan():
    """Asks Windows to scan the air again (WlanScan), so the list is up to date. Best effort."""
    try:
        import ctypes
        from ctypes import wintypes

        class GUID(ctypes.Structure):
            _fields_ = [("d1", wintypes.DWORD), ("d2", wintypes.WORD), ("d3", wintypes.WORD), ("d4", ctypes.c_ubyte * 8)]

        class INFO(ctypes.Structure):
            _fields_ = [("guid", GUID), ("desc", ctypes.c_wchar * 256), ("state", wintypes.DWORD)]

        class LIST(ctypes.Structure):
            _fields_ = [("n", wintypes.DWORD), ("i", wintypes.DWORD), ("items", INFO * 8)]

        w = ctypes.windll.wlanapi
        h, ver = wintypes.HANDLE(), wintypes.DWORD()
        if w.WlanOpenHandle(2, None, ctypes.byref(ver), ctypes.byref(h)) != 0:
            return
        lst = ctypes.POINTER(LIST)()
        if w.WlanEnumInterfaces(h, None, ctypes.byref(lst)) == 0:
            for k in range(min(lst.contents.n, 8)):
                w.WlanScan(h, ctypes.byref(lst.contents.items[k].guid), None, None, None)
            w.WlanFreeMemory(lst)
        w.WlanCloseHandle(h, None)
        time.sleep(4)  # Windows needs a few seconds to collect the answers
    except Exception:  # noqa: BLE001 - the cached list is still fine
        pass


def networks():
    """[(ssid, authentication, signal)] the laptop sees, strongest first, one entry per name."""
    nets, ssid, auth, sig = {}, None, "", 0
    def keep():
        if ssid and (ssid not in nets or nets[ssid][1] < sig):
            nets[ssid] = (auth, sig)
    for line in netsh("show", "networks", "mode=bssid").splitlines():
        m = re.match(r"^SSID \d+ : (.*)$", line)
        if m:
            keep()
            ssid, auth, sig = m.group(1).strip(), "", 0
            continue
        m = re.match(r"^\s+Authentication\s+:\s(.*)$", line)
        if m:
            auth = m.group(1).strip()
        m = re.match(r"^\s+Signal\s+:\s(\d+)%", line)
        if m:
            sig = max(sig, int(m.group(1)))
    keep()
    return sorted(((s, a, g) for s, (a, g) in nets.items()), key=lambda n: -n[2])


def scan():
    fresh_scan()
    cur = current_ssid()
    out = []
    for s, a, g in networks():
        if s and len(s.encode()) <= 32 and "\t" not in s:
            out.append(f"NET\t{g}\t{0 if a == 'Open' else 1}\t{1 if s == cur else 0}\t{s}")
    return out + ["END"]


def profile_xml(ssid, auth, enc, password):
    x = lambda v: html.escape(v, quote=True)
    key = "" if auth == "open" else (
        f"<sharedKey><keyType>passPhrase</keyType><protected>false</protected><keyMaterial>{x(password)}</keyMaterial></sharedKey>")
    return f"""<?xml version="1.0"?>
<WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1"><name>{x(ssid)}</name>
<SSIDConfig><SSID><hex>{ssid.encode().hex().upper()}</hex><name>{x(ssid)}</name></SSID></SSIDConfig>
<connectionType>ESS</connectionType><connectionMode>manual</connectionMode><MSM><security><authEncryption>
<authentication>{auth}</authentication><encryption>{enc}</encryption><useOneX>false</useOneX></authEncryption>
{key}</security></MSM></WLANProfile>"""


def add_profile(xml):
    import os
    import tempfile
    fd, path = tempfile.mkstemp(suffix=".xml")
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write(xml)
    try:
        return netsh_ok("add", "profile", f"filename={path}", "user=current")
    finally:
        os.remove(path)  # it held the password in plain text


def export_profile(ssid):
    """The laptop's saved profile for `ssid` (XML with the key), to put back if a new password fails."""
    import glob
    import os
    import tempfile
    d = tempfile.mkdtemp()
    try:
        netsh("export", "profile", f"name={ssid}", "key=clear", f"folder={d}")
        files = glob.glob(os.path.join(d, "*.xml"))
        return open(files[0], encoding="utf-8").read() if files else None
    finally:
        for f in glob.glob(os.path.join(d, "*")):
            os.remove(f)
        os.rmdir(d)


def wait_for(ssid, seconds):
    for _ in range(seconds * 2):
        time.sleep(0.5)
        if current_ssid() == ssid:
            return True
    return False


# Windows profile settings for each kind of network, tried in order.
SECURITY = {
    "Open": [("open", "none")],
    "WPA3-Personal": [("WPA3SAE", "AES"), ("WPA2PSK", "AES")],
    "WPA2-Personal": [("WPA2PSK", "AES")],
    "WPA-Personal": [("WPAPSK", "AES"), ("WPAPSK", "TKIP")],
}


def connect(ssid, password):
    """Makes the LAPTOP join `ssid`. Works for networks the laptop has never joined (a profile is made from the
    password); if joining fails, the laptop goes back to the network it was on."""
    before = current_ssid()
    if ssid == before and not password:
        return f"OK the laptop is already on {ssid}"
    seen = {s: a for s, a, _ in networks()}
    if ssid not in seen:
        fresh_scan()
        seen = {s: a for s, a, _ in networks()}
    if password and len(password) < 8:
        return "ERR a Wi-Fi password has at least 8 characters"
    known = netsh_ok("show", "profiles", f"name={ssid}")
    if ssid not in seen and not password and not known:
        return f"ERR the laptop cannot see {ssid} now: check the name (capitals count) and run wifi scan"
    # A network that drops out of the list for a moment (weak signal) is still tried, as WPA2/WPA3.
    auth = seen.get(ssid, "WPA3-Personal" if password else "Open")
    if "Enterprise" in auth:
        return f"ERR {ssid} uses a username login ({auth}), which only Windows' own Wi-Fi menu can do"
    if not password and not known and auth != "Open":
        return f"ERR {ssid} needs its password: wifi connect {ssid} <password>"
    backup = export_profile(ssid) if known and password else None
    tries = SECURITY.get(auth, [("WPA2PSK", "AES")]) if (password or not known) else [None]
    for t in tries:
        if t is not None:
            netsh("delete", "profile", f"name={ssid}")
            if not add_profile(profile_xml(ssid, t[0], t[1], password)):
                continue
        # netsh fails at once when Windows cannot even try (network out of range); no point waiting then.
        if netsh_ok("connect", f"name={ssid}", f"ssid={ssid}") and wait_for(ssid, 15):
            return f"OK the laptop joined {ssid}"
    # Failed: undo the profile change and go back to where the laptop was.
    netsh("delete", "profile", f"name={ssid}")
    if backup:
        add_profile(backup)
    if before and before != ssid:
        netsh("connect", f"name={before}")
        wait_for(before, 10)
    back = f"; back on {before}" if before and current_ssid() == before else ""
    return f"ERR could not join {ssid}: wrong password or weak signal{back}"


def wifi_loop():
    while True:
        try:
            f = open(PIPE, "r+b", buffering=0)
        except OSError:
            time.sleep(2)
            continue
        print("[wifi] connected to the BedrockOS VM (COM2)", flush=True)
        buf = b""
        try:
            while True:
                ch = f.read(1)
                if not ch:
                    break
                if ch != b"\n":
                    buf += ch
                    continue
                fields = buf.rstrip(b"\r").decode("utf-8", "replace").split("\t")
                buf = b""
                cmd = fields[0]
                if cmd == "HELLO":
                    lines = ["AEONWIFI 1"]
                elif cmd == "SCAN":
                    lines = scan()
                    print(f"[wifi] scan: {len(lines) - 1} networks", flush=True)
                elif cmd == "CONNECT" and len(fields) >= 2:
                    lines = [connect(fields[1], fields[2] if len(fields) > 2 else "")]
                    print(f"[wifi] connect {fields[1]}: {lines[0]}", flush=True)
                elif cmd:
                    lines = ["ERR unknown command"]
                else:
                    continue
                # The protocol separates fields with tabs: "OK<tab>text", "ERR<tab>text".
                lines = [re.sub(r"^(OK|ERR) ", "\\1\t", l) for l in lines]
                f.write(("\n".join(lines) + "\n").encode("utf-8"))
        except OSError as e:
            print(f"[wifi] pipe closed ({e}); waiting for the VM", flush=True)
        try:
            f.close()
        except OSError:
            pass
        time.sleep(1)


# ---- Web -----------------------------------------------------------------------------------------------------

STRIP = re.compile(rb"<(script|style|noscript|svg)\b.*?</\1\s*>", re.S | re.I)


class Web(BaseHTTPRequestHandler):
    def do_GET(self):
        q = urllib.parse.urlparse(self.path)
        url = urllib.parse.parse_qs(q.query).get("u", [""])[0]
        if q.path != "/fetch" or not url.startswith(("http://", "https://")):
            return self.reply(400, b"use /fetch?u=<http or https address>", "")
        try:
            req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "text/html,*/*;q=0.5", "Accept-Encoding": "identity"})
            with urllib.request.urlopen(req, timeout=20) as r:
                body = r.read(MAX_PAGE * 3)
                final = r.geturl()
                charset = r.headers.get_content_charset() or "utf-8"
                ctype = r.headers.get_content_type()
        except urllib.error.HTTPError as e:
            body, final, charset, ctype = e.read(MAX_PAGE), url, "utf-8", "text/html"
        except Exception as e:  # noqa: BLE001 - every failure becomes a page for the browser
            return self.reply(502, str(e).encode(), url)
        if not ctype.startswith("text/"):
            body = f"<h1>Not a web page</h1><p>{html.escape(final)} is {html.escape(ctype)}, which Crafting Table cannot show.</p>".encode()
        body = STRIP.sub(b"", body)
        text = body.decode(charset, "replace")[:MAX_PAGE]
        print(f"[web] {final} ({len(text)} chars)", flush=True)
        self.reply(200, text.encode("utf-8"), final)

    def reply(self, code, data, final):
        self.send_response(code)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("X-Final-Url", final)
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *a):
        pass


def main():
    threading.Thread(target=wifi_loop, daemon=True).start()
    srv = ThreadingHTTPServer(("127.0.0.1", WEB_PORT), Web)
    print(f"BedrockOS helper: Wi-Fi on {PIPE}, web pages on port {WEB_PORT}. Ctrl+C stops it.", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        sys.exit(0)


if __name__ == "__main__":
    main()
