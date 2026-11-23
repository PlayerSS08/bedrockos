# BedrockOS status

Legend: **done** = built and checked as stated · **partial** = some of it works, the rest is listed ·
**untested on hardware** = checked in VirtualBox only · **inherited** = Aeon 0.11.0 code, carried over,
unit-tested on the host, not yet re-verified in BedrockOS beyond booting · **not done**.

No feature has been tested on real hardware (USB stick on a PC) yet.

Last update: 2026-11-23, milestone 3 (part 6: ChatBox window).

| Feature | Status | How it was checked |
|---|---|---|
| One-command build (`cargo xtask build`): `.efi`, `out/usb/`, `.img`, `.vdi` | done | Run on Windows 11 with Rust 1.100, Python 3.14, VirtualBox 7.2.22 |
| Disk image: GPT, FAT32 ESP + empty FAT32 `BRDATA` data partition | done | Booted by VirtualBox EFI; data partition not used by the OS yet |
| VirtualBox scripts (`scripts/vbox.ps1`, `.bat`, `.sh`) | done (Windows); `.sh` untested | `vbox.ps1` created the VM and booted it; `vbox.sh` written, not run (no Linux/macOS host here) |
| Boot in VirtualBox to the shell, 1024x768 and 1920x1080 | done, untested on hardware | `tests/vbox_boot.py`: 10/10 checks (serial markers, no panic, screenshot size, non-blank) |
| Screen mode: keep firmware's mode when it is ≥1024x720 | done | unit test + VirtualBox at both sizes (before: VirtualBox's 7680x4320 was chosen) |
| Host unit tests | done | `cargo xtask test`: 724 passed, 3 ignored (preview image writers) |
| Clippy `-D warnings` (UEFI build and host library) | done | `cargo xtask clippy` clean |
| Kernel: paging W^X, LAPIC timer, preemptive scheduler, ring 3, fork/exec/pipes | inherited | boots in VirtualBox (`lapic timer ok hz=100`, scheduler tasks start) |
| e1000 driver | inherited | bound in VirtualBox (`net bound Intel e1000 (8086:100e)`); DHCP not yet re-checked |
| AHCI disk | inherited | VirtualBox disk found (`ahci disk on port 0: VBOX HARDDISK`) |
| xHCI / USB HID | inherited | controller up in VirtualBox; VirtualBox USB tablet reported "not supported" (absolute mouse, to do in M3) |
| Animated boot: starfield, hexagon logo with glow and light sweep, wordmark, progress bar + live status | done, untested on hardware | VirtualBox 1024x768: screenshots show it before and after ExitBootServices; host preview images at 1024x768 and 1920x1080 |
| Progress tied to real boot stages (13 stages, from firmware tables to filesystem) | done | serial log shows each stage; bar values come from `bootfx::stage` calls in `main.rs` |
| Frame rate (target 30 fps) | done in VirtualBox | serial `bootfx frame` lines. 1024x768: full frame 8 ms, normal 5 ms. 1920x1080: full 10 ms, normal 8–20 ms, menu about 17 ms; measured 16 frames per 0.58 s (about 28 fps) in the menu |
| No flicker: off-screen back buffer, only changed rectangles copied | done | design + VirtualBox screenshots; flicker itself not measurable from screenshots |
| Boot menu: Boot, Recovery, Safe mode (no network), Firmware setup, Restart, Power off, with countdown ring | done (drawn, countdown seen) | VirtualBox screenshot of the menu counting down; Boot and Safe mode chosen with keys in VirtualBox; Recovery, Firmware setup, Restart, Power off unit-tested in `menulogic` only, **not run in VirtualBox** |
| Esc / V switches to the scrolling boot log | done (Esc) | VirtualBox: Esc sent with `keyboardputscancode` after leaving the menu; log line `switched to boot messages`, screenshot shows the log and shell. V not separately tested |
| Safe mode skips network probing | done | VirtualBox: Down, Down, Enter in the menu; log shows `safe mode selected` and `network drivers not started` |
| Fade out at the end of boot | done | VirtualBox screenshots show the dimmed frame, then the shell |
| Fade into the desktop | done | VirtualBox screenshots: boot screen fades out, desktop fades in (about 0.3 s) |
| Desktop: wallpaper, top panel with clock, Terminal window | done, untested on hardware | VirtualBox at 1024x768 and 1920x1080: screenshots; `help` typed with key injection shows its output in the window |
| Shell inside the Terminal window (keyboard input, scrolling, status bar) | done | VirtualBox: inherited console placed in the window rectangle |
| Panel clock | partial: shows **UTC** (no time zone setting yet), updates every minute | VirtualBox screenshot; minute change not watched |
| Move the Terminal window by dragging its title bar | done | VirtualBox: pointer moved onto the title bar, button pressed, dragged, released (all injected with `PutMouseEvent`); screenshots show the window in its new place with its text redrawn, and typed output appears there |
| Dock: seven app icons (Notes, Tasks, Browser, Terminal, Wi-Fi, Settings, Help) at the bottom; clicking one shows a panel message for about 3 s ("Terminal is open", others "<App> arrives in milestone N") | done, untested on hardware; the apps themselves are **not done** | VirtualBox: Notes clicked at 1920x1080 and Terminal at 1024x768 (pointer moved and clicked with `PutMouseEvent`); screenshots show the message, and 4 s later it is gone. Unit tests: icon hit areas, message texts and widths, window cannot be dragged over the dock |
| ChatBox window (was "Terminal"): own title bar with the bedrock icon from `assets/bedrock.jpg` (converted by `tools/genicon.py`, background removed), name on the left, minimise / maximise / close on the right, small corner radius | done, untested on hardware | Host preview images and VirtualBox screenshot at 1920x1080; unit tests for the icon data and button positions |
| Window manager: resize, focus, several windows, close/minimise buttons | not done (the minimise, maximise and close buttons are drawn but do nothing yet) | rest of milestone 3 |
| USB mouse in VirtualBox | done | Fixed the inherited xHCI driver: VirtualBox's USB mouse stalls SET_PROTOCOL and SET_IDLE; endpoint 0 is now reset after a stall and the mouse is used anyway. Checked: `USB mouse ready` in the log; movement injected with the VirtualBox COM API (`PutMouseEvent`) arrives as reports and moves the pointer (`mouse` command and screenshot) |
| Mouse pointer on the whole desktop | done | VirtualBox: movement injected with `PutMouseEvent`; screenshots show the pointer inside the Terminal window (drawn by the console) and on the top panel (drawn by the desktop), with nothing left behind when it crosses between them. Clicks are read but do nothing yet. Inherited glitch: a small pointer fragment can stay behind when text is drawn under the pointer |
| VirtualBox USB tablet (absolute pointer, the VirtualBox default) | not done: no tablet driver, so the scripts set the VM to a USB mouse (click into the VM window to capture it, Right Ctrl releases) | — |
| Apps: Notes, Tasks, Browser, Wi-Fi, Settings | not done | milestones 4–10 |
| Notes / tasks persistence | not done | milestone 5–6 |
| Network manager, own Wi-Fi, simulated AP | not done | milestone 8 |
| Wi-Fi chip drivers | not done: **no supported chip yet** | milestone 8 |
| Browser, HTTPS | not done | milestone 9 |
| Apps as ring-3 programs | not done: apps will run as kernel tasks first | — |
