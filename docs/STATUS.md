# BedrockOS status

Legend: **done** = built and checked as stated · **partial** = some of it works, the rest is listed ·
**untested on hardware** = checked in VirtualBox only · **inherited** = Aeon 0.11.0 code, carried over,
unit-tested on the host, not yet re-verified in BedrockOS beyond booting · **not done**.

No feature has been tested on real hardware (USB stick on a PC) yet.

Last update: 2026-11-23, milestone 1.

| Feature | Status | How it was checked |
|---|---|---|
| One-command build (`cargo xtask build`): `.efi`, `out/usb/`, `.img`, `.vdi` | done | Run on Windows 11 with Rust 1.100, Python 3.14, VirtualBox 7.2.22 |
| Disk image: GPT, FAT32 ESP + empty FAT32 `BRDATA` data partition | done | Booted by VirtualBox EFI; data partition not used by the OS yet |
| VirtualBox scripts (`scripts/vbox.ps1`, `.bat`, `.sh`) | done (Windows); `.sh` untested | `vbox.ps1` created the VM and booted it; `vbox.sh` written, not run (no Linux/macOS host here) |
| Boot in VirtualBox to the shell, 1024x768 and 1920x1080 | done, untested on hardware | `tests/vbox_boot.py`: 10/10 checks (serial markers, no panic, screenshot size, non-blank) |
| Screen mode: keep firmware's mode when it is ≥1024x720 | done | unit test + VirtualBox at both sizes (before: VirtualBox's 7680x4320 was chosen) |
| Host unit tests | done | `cargo xtask test`: 677 passed, 1 ignored |
| Clippy `-D warnings` (UEFI build and host library) | done | `cargo xtask clippy` clean |
| Kernel: paging W^X, LAPIC timer, preemptive scheduler, ring 3, fork/exec/pipes | inherited | boots in VirtualBox (`lapic timer ok hz=100`, scheduler tasks start) |
| e1000 driver | inherited | bound in VirtualBox (`net bound Intel e1000 (8086:100e)`); DHCP not yet re-checked |
| AHCI disk | inherited | VirtualBox disk found (`ahci disk on port 0: VBOX HARDDISK`) |
| xHCI / USB HID | inherited | controller up in VirtualBox; VirtualBox USB tablet reported "not supported" (absolute mouse, to do in M3) |
| Animated boot, boot menu restyle, Safe mode | not done (Aeon splash and menu still in place) | milestone 2 |
| Desktop GUI, window manager, apps | not done | milestones 3–10 |
| Notes / tasks persistence | not done | milestone 5–6 |
| Network manager, own Wi-Fi, simulated AP | not done | milestone 8 |
| Wi-Fi chip drivers | not done: **no supported chip yet** | milestone 8 |
| Browser, HTTPS | not done | milestone 9 |
| Apps as ring-3 programs | not done: apps will run as kernel tasks first | — |
