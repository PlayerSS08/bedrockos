#!/usr/bin/env python3
"""Uploads the bootable BedrockOS ISO to Hugging Face, with a page (README) describing it.

    hf auth login                       (once: paste a token with Write access)
    python tools/publish_hf.py [--private]

Creates (or updates) the repository <your user name>/BedrockOS and prints its URL.
"""
import argparse
import hashlib
import os
import sys

from huggingface_hub import HfApi

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ISO = os.path.join(ROOT, "out", "bedrockos.iso")

CARD = """---
license: other
tags:
  - operating-system
  - rust
  - uefi
  - iso
---

# BedrockOS

A UEFI operating system written from scratch in Rust, with a Minecraft-inspired look.
Not Linux, not Debian: its own kernel, drivers, network stack, file system, desktop and apps.

**Download:** [`bedrockos.iso`](bedrockos.iso) ({size_mb:.0f} MB, SHA-256 `{sha}`)

## Run it in VirtualBox

1. New VM: type *Other*, version *Other/Unknown (64-bit)*, 1024 MB RAM, no hard disk.
2. Settings → System → tick **Enable EFI**.
3. Settings → Storage → optical drive → choose `bedrockos.iso`.
4. Start. Sign in with user name **BedrockOS**, password **MinecraftSS_08**, then press Enter on *Boot BedrockOS*.

It also boots on real UEFI PCs from a CD/DVD (Secure Boot off). The CD is read-only, so notes and
tasks are not kept between boots.

## Features

- Sign-in screen and boot menu over a Minecraft cave background, with yellow splash text
- Desktop with windows, dock, maximise, top bar with clock, battery and hover tips
- Apps: ChatBox (shell with 100+ commands), Notes, Tasks, Files, Calculator, Clock, Snake,
  Settings, Help, Network and the **Crafting Table** web browser
- Minecraft-style "Achievement get!" toasts and a wallpaper that follows day and night
- Networking: DHCP, DNS, TCP, HTTP, ping, curl
- Wi-Fi and https pages through the host laptop when run in VirtualBox (`tools/bedrock-host.py`)
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--private", action="store_true")
    a = ap.parse_args()
    if not os.path.exists(ISO):
        sys.exit("out/bedrockos.iso is missing: run `cargo xtask build` first")
    api = HfApi()
    user = api.whoami()["name"]
    repo = f"{user}/BedrockOS"
    api.create_repo(repo, repo_type="model", private=a.private, exist_ok=True)
    data = open(ISO, "rb").read()
    card = CARD.format(size_mb=len(data) / (1024 * 1024), sha=hashlib.sha256(data).hexdigest())
    api.upload_file(path_or_fileobj=card.encode(), path_in_repo="README.md", repo_id=repo, commit_message="BedrockOS page")
    api.upload_file(path_or_fileobj=ISO, path_in_repo="bedrockos.iso", repo_id=repo, commit_message="BedrockOS bootable ISO")
    print(f"page: https://huggingface.co/{repo}")
    print(f"ISO:  https://huggingface.co/{repo}/resolve/main/bedrockos.iso")


if __name__ == "__main__":
    main()
