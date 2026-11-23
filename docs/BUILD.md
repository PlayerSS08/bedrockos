# Building and running BedrockOS

## Requirements

| Tool | Why | Install |
|---|---|---|
| Rust stable (1.86+) with `x86_64-unknown-uefi` | the kernel | `rustup target add x86_64-unknown-uefi` |
| Host target for tests (Windows: `x86_64-pc-windows-gnu`) | `cargo xtask test` | `rustup target add x86_64-pc-windows-gnu` (Linux/macOS: your default host target) |
| Python 3 (standard library only) | disk image builder | python.org / winget `Python.Python.3` |
| VirtualBox 7.x | the `.vdi`, the VM, boot tests | `winget install Oracle.VirtualBox` |

No `make` is needed; `cargo xtask` is the build driver.

## Commands

```sh
cargo xtask build     # out/bedrockos.efi, out/usb/, out/bedrockos.img, out/bedrockos.vdi
cargo xtask test      # host unit tests
cargo xtask clippy    # clippy -D warnings, UEFI and host
cargo xtask all       # all three
python tests/vbox_boot.py   # VirtualBox boot test at 1024x768 and 1920x1080 (screenshots in out/test-shots/)
```

## Run in VirtualBox

Windows: `scripts\vbox.bat` (or `powershell -File scripts\vbox.ps1`). Linux/macOS: `sh scripts/vbox.sh`.

The script builds if needed, creates the VM `BedrockOS` (EFI, 1 GB RAM, 2 CPUs, VMSVGA, Intel e1000 on NAT,
xHCI USB, COM1 → `out/serial.log`), attaches `out/bedrockos.vdi` and starts it.

| Option (ps1 / sh) | Meaning |
|---|---|
| `-Res 1920x1080` / `--res 1920x1080` | screen size the firmware starts in (BedrockOS keeps it) |
| `-Headless` / `--headless` | no window |
| `-Net nat|bridged|intnet|none` / `--net ...` | network mode (see `NETWORK.md` once milestone 8 lands) |
| `-Action stop|delete|screenshot|create` / `stop` ... | other actions |
| `-Build` / `--build` | rebuild first |

The host key (right Ctrl) releases keyboard and mouse back to the host.

## Boot options

`EFI/BOOT/bedrock.cfg` on the boot disk (source: `config/bedrock.cfg`), one option per line:
`quiet`, `debug`, `verbose`, `nomenu`, `timeout=N`, `mode=WxH`, `recovery`, `nocount`, `nolapic`.

## Real hardware

See `USB.md`.
