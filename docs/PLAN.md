# BedrockOS plan

BedrockOS is a small x86_64 UEFI operating system in Rust (`no_std`, no third-party crates in the kernel).
It starts from **Bedrock 0.11.0** (kept read-only in `../reference/`), which already has a tested kernel, drivers,
TCP/IP stack and shell, and adds the missing pieces: a real animated boot, a graphical desktop with apps,
persistence for notes and tasks, an OS-owned Wi-Fi stack with swappable backends, a network manager, and a browser.

Test targets: **Oracle VirtualBox** (primary, scripted with `VBoxManage`) and **real UEFI hardware from a USB
stick**. QEMU is not used anywhere.

## 1. Architecture

```
 UEFI firmware
     │  efi_main (src/main.rs)
     ▼
 ┌─────────────── boot stage (firmware still active) ────────────────┐
 │ GOP framebuffer · boot options (bedrock.cfg) · boot menu ·        │
 │ animated splash driven by real stages · TPM measure · seed        │
 └──────────────────────────────┬────────────────────────────────────┘
                                │ ExitBootServices, BootInfo hand-over
 ┌──────────────────────────────▼──── kernel ────────────────────────┐
 │ GDT/IDT/TSS · W^X paging · frames + 32 MB heap · LAPIC timer ·    │
 │ preemptive scheduler · syscalls · ring-3 processes (fork/exec)    │
 ├──────────────┬───────────────┬───────────────┬────────────────────┤
 │ drivers      │ net           │ fs            │ gui                │
 │ ps2, xhci,   │ netstack, tcp │ ramfs, fat    │ compositor (double │
 │ hid kbd/mouse│ dhcp, dns,    │ reader, fatw  │ buffer, dirty      │
 │ ahci, vblk,  │ http, tls*    │ writer, atomic│ rects), fonts,     │
 │ e1000, virtio│ netmgr*       │ save*, /notes │ theme, widgets,    │
 │ rtl8168, usb │               │ /tasks /config│ window manager,    │
 │ net, wifi/*  │               │               │ top bar, dock      │
 ├──────────────┴───────────────┴───────────────┴────────────────────┤
 │ apps*: terminal · notes · tasks · browser · wifi · settings · help│
 │ shell: command table (one source for help, completion, Help app)  │
 └───────────────────────────────────────────────────────────────────┘
   * = new in BedrockOS
```

The GUI runs as a kernel task driven by the timer and input queues; network polling and saving run as
their own scheduler tasks, so the desktop never blocks the kernel. Apps are written against a small
`App` trait (`event`, `draw`, `tick`) so they can later move to ring 3; in the first version they run as
kernel tasks (recorded in `docs/STATUS.md`).

### Layout decision

The prompt suggests `src/boot`, `src/kernel`, `src/drivers`, … Bedrock is one flat crate of 112 modules whose
pure parts compile and are unit-tested on the host (`lib.rs`), with hardware parts gated on
`target_os = "uefi"`. That split is the valuable part and is kept. **New subsystems go straight into
folders** (`src/gui/`, `src/apps/`, `src/net/netmgr.rs`, `src/drivers/wifi/`), and existing modules move into
folders when a milestone rewrites them. Moving all 112 files on day one would gain nothing and would break
the source-scanning tests (for example the command-table test reads `shell.rs`).

## 2. Module list: reuse, adapt, new

| Area | From Bedrock (reuse as is) | Adapt | New |
|---|---|---|---|
| Boot | `uefi`, `bootinfo`, `bootopts`, `bootcount`, `tcg2`, `eventlog`, `entropy` | `main` (stages), `menulogic` (Safe mode entry, no-network), `splash`, `anim`, `gopmode` (keep firmware mode) | `boot/anim` procedural logo, glow, starfield, verbose toggle |
| Kernel | `arch`, `apic`, `paging`, `frames`, `heapalloc`, `mm`, `sched`, `syscall`, `usermode`, `vmspace`, `loader`, `elf`, `pipe`, `waitq`, `spin` | `panicrec` + `fb` stop screen restyled | per-task CPU accounting for the task-manager view |
| Input | `keymap`, `hidkbd`, `hidparse`, `pointer`, `touchpad`, `i2chid`, `xhci*` | | PS/2 mouse, input event queue for the GUI |
| Storage | `fat`, `fatname`, `fatw`, `bcache`, `ahci`, `vblk`, `block`, `usbmsd`, `usbfs` | volume label rule (`BEDROCK` → `BRDATA`) | `fs/atomic` (temp + rename), `/notes /tasks /config` store |
| Network | `netstack`, `tcp`, `dns`, `http`, `e1000`, `virtionet`, `rtl8168`, `usbnet` | | `net/netmgr` (interfaces, default route, failover), HTTP redirects/chunked/gzip, `net/tls` |
| Wi-Fi | `ieee80211`, `wpa`, `wifi` (state machine) | `rtw89` stub kept as "not supported" | `drivers/wifi/` trait; backend A (USB chip: ath9k_htc or mt7601u), backend B simulated AP over intnet, backend C bridges |
| GUI | `gfont`, `fontdata`, `pixel`, `palette` | `tools/genfont.py` (more sizes) | `gui/`: canvas, AA shapes, theme (dark/light), widgets, window manager, top bar, dock, palette (Ctrl+Space) |
| Shell | `shell`, `shparse`, `lineedit`, `textcmd`, `cmdhelp`, `editbuf` | output to a terminal widget instead of the whole screen | new commands from section 5 |
| Apps | | | terminal, notes, tasks (+ system view), browser, wifi, settings, help |
| Tools | `genfont.py`, `pack_initfs.py` | | `xtask`, `tools/mkimage.py`, `scripts/vbox.*`, `tests/vbox_boot.py`, `tools/mock_ap.py` |

## 3. Screen mockups

Boot (real stages drive the bar; `Esc`/`V` swaps to the scrolling log):

```
┌──────────────────────────────────────────────────────────────┐
│ ·  ˙    ·        slow starfield / gradient       ·     ˙     │
│                                                              │
│                         ◢◣                                   │
│                       ◢████◣      glow pulses                │
│                     ◢████████◣                               │
│                      B E D R O C K O S                       │
│                                                              │
│              ━━━━━━━━━━━━━━━━━━━━━──────────────             │
│              Starting network (e1000)…            62 %       │
│                                         Esc: show details    │
└──────────────────────────────────────────────────────────────┘
```

Boot menu:

```
┌──────────────────────────────────────────────────────────────┐
│                      B E D R O C K O S                       │
│        ┌────────────────────────────────────────────┐        │
│        │ ▶ Boot BedrockOS                           │        │
│        │   Recovery                                 │        │
│        │   Safe mode (no network)                   │        │
│        │   Firmware setup                           │        │
│        │   Restart                                  │        │
│        │   Power off                                │        │
│        └────────────────────────────────────────────┘        │
│              Booting in 5 s  ◔   ↑↓ choose · Enter           │
└──────────────────────────────────────────────────────────────┘
```

Desktop:

```
┌ BedrockOS ───────────────────────────── ▂▄▆ 10.0.2.15  ⌨ Caps  14:32 ┐
│                                                                      │
│   ┌ Notes ───────────────── _ □ × ┐   ┌ Terminal ─────── _ □ × ┐     │
│   │ ▸ Shopping     │ Milk         │   │ bedrock> wifi scan     │     │
│   │   Ideas        │ Bread_       │   │ [simulated] 3 networks │     │
│   │   Journal      │              │   │ 1 HomeNet  ▂▄▆ WPA2    │     │
│   │ + New          │  ● saved     │   │ bedrock> _             │     │
│   └──────────────────────────────┘   └────────────────────────┘     │
│                 procedural wallpaper (gradient + soft shapes)        │
│        ╭──────────────────────────────────────────────────╮          │
│        │  📝   ✓   🌐   >_   📶   ⚙   ?                     │  dock   │
│        ╰──────────────────────────────────────────────────╯          │
└──────────────────────────────────────────────────────────────────────┘
```
(Icons are drawn as vector shapes, not emoji; the emoji above only stand in for them.)

Wi-Fi app:

```
┌ Wi-Fi ─────────────────────────────────────────── _ □ × ┐
│ Host connection (laptop Wi-Fi)                ● active   │
│   e1000 · NAT · 10.0.2.15 · gw 10.0.2.2 · dns 10.0.2.3   │
│   The laptop chooses the Wi-Fi network in this mode.     │
│──────────────────────────────────────────────────────────│
│ OS Wi-Fi  [simulated]                       [ Power ● ]  │
│   ▂▄▆█ BedrockTest-WPA2   ch 6   -42 dBm  WPA2  Connect  │
│   ▂▄▆  OpenCafe           ch 11  -61 dBm  open  Connect  │
│   Saved: BedrockTest-WPA2 (Forget)     Autoconnect: on   │
│ Default route: [ auto ▾ ]   priority: wifi, host         │
└──────────────────────────────────────────────────────────┘
```

## 4. Milestones

1. Skeleton: builds, boots in VirtualBox, build + VM scripts, this plan. **(done, see STATUS.md)**
2. Boot experience: animated boot tied to real stages, verbose toggle, menu with Safe mode, fade to desktop.
3. GUI core: compositor, fonts, theme, widgets, window manager, top bar, dock, palette, mouse + keyboard.
4. Terminal app + shell in a window, full-screen on F12, new commands.
5. Storage: `BRDATA` FAT32 data partition, atomic saves, `/notes /tasks /config`.
6. Notes + Tasks apps with persistence, system task-manager view.
7. Networking in the GUI, `nettest`.
8. Wi-Fi trait, network manager, simulated AP backend + mock AP tool, Wi-Fi app, then a USB chip driver.
9. Browser: HTTP, HTML subset, tabs, bookmarks; then TLS as its own sub-milestone.
10. Polish, extras, settings, help, performance, release images.

## 5. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Real Wi-Fi chip drivers (ath9k_htc / mt7601u over xHCI, RTL8852BE) are large and need real hardware | Own Wi-Fi may only work in the simulated backend | Trait + simulated backend first, so the UI and commands are complete; USB chip driver with VirtualBox USB passthrough; docs state exactly what was verified |
| TLS 1.3 with certificate checking is a lot of crypto code (X25519, AES-GCM/ChaCha20, RSA/ECDSA verify, X.509) | HTTPS may arrive late | Separate milestone after everything else; until then the browser shows an honest "HTTPS not supported yet" page and never falls back to HTTP |
| Kernel image size grows (Bedrock's CI limit was 768 KiB) | Slower boot, but no functional limit | Size reported every build; budget raised deliberately in STATUS.md |
| Redrawing large framebuffers at 30 fps without a GPU | Choppy UI | Double buffer in RAM, dirty-rectangle copies, keep firmware's mode (1024x768 / 1920x1080 in VMs) |
| VirtualBox's mouse is absolute (USB tablet) | Relative-only HID code mis-tracks | Add absolute HID reports (digitizer) alongside the relative mouse |
| No real-hardware test in this environment | Hardware claims unverifiable | STATUS.md lists every feature as VirtualBox-tested or "not tested on hardware" |
| VM alternatives: Windows 11 Home has no Hyper-V; QEMU is excluded; VMware Workstation Pro needs a manual Broadcom download | Only one hypervisor available here | VirtualBox scripted; VMware `.vmx` + `vmrun` scripts can be added and marked untested until it is installed |
