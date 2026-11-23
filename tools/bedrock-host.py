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
    return r.stdout.decode("utf-8", "replace") if r.returncode == 0 else r.stdout.decode("utf-8", "replace") + r.stderr.decode("utf-8", "replace")


def current_ssid():
    m = re.search(r"^\s+SSID\s+:\s(.*)$", netsh("show", "interfaces"), re.M)
    return m.group(1).strip() if m else ""


def scan():
    cur = current_ssid()
    nets, ssid, auth, sig = [], None, "", 0
    for line in netsh("show", "networks", "mode=bssid").splitlines():
        m = re.match(r"^SSID \d+ : (.*)$", line)
        if m:
            if ssid:
                nets.append((ssid, auth, sig))
            ssid, auth, sig = m.group(1).strip(), "", 0
            continue
        m = re.match(r"^\s+Authentication\s+:\s(.*)$", line)
        if m:
            auth = m.group(1).strip()
        m = re.match(r"^\s+Signal\s+:\s(\d+)%", line)
        if m:
            sig = max(sig, int(m.group(1)))
    if ssid:
        nets.append((ssid, auth, sig))
    out = []
    for s, a, g in sorted(nets, key=lambda n: -n[2]):
        if s and len(s.encode()) <= 32 and "\t" not in s:
            out.append(f"NET\t{g}\t{0 if a == 'Open' else 1}\t{1 if s == cur else 0}\t{s}")
    return out + ["END"]


def connect(ssid, password):
    if ssid == current_ssid():
        return f"OK the laptop is already on {ssid}"
    profiles = netsh("show", "profiles")
    if not re.search(r":\s" + re.escape(ssid) + r"\s*$", profiles, re.M):
        if not password:
            return "ERR this network needs a password: wifi connect <name> <password>"
        x = lambda s: html.escape(s, quote=True)
        xml = f"""<?xml version="1.0"?>
<WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1"><name>{x(ssid)}</name>
<SSIDConfig><SSID><hex>{ssid.encode().hex().upper()}</hex><name>{x(ssid)}</name></SSID></SSIDConfig>
<connectionType>ESS</connectionType><connectionMode>manual</connectionMode><MSM><security><authEncryption>
<authentication>WPA2PSK</authentication><encryption>AES</encryption><useOneX>false</useOneX></authEncryption>
<sharedKey><keyType>passPhrase</keyType><protected>false</protected><keyMaterial>{x(password)}</keyMaterial>
</sharedKey></security></MSM></WLANProfile>"""
        import os
        import tempfile
        fd, path = tempfile.mkstemp(suffix=".xml")
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(xml)
        try:
            netsh("add", "profile", f"filename={path}")
        finally:
            os.remove(path)
    r = netsh("connect", f"name={ssid}")
    for _ in range(30):
        time.sleep(0.5)
        if current_ssid() == ssid:
            return f"OK the laptop joined {ssid}"
    return "ERR the laptop could not join " + ssid + ": " + " ".join(r.split())[:60]


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
