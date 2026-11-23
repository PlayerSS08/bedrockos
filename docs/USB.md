# Booting BedrockOS from a USB stick

**Status: not yet tested on real hardware.** These steps follow the UEFI removable-media rules the image is
built for; report what you see.

## Option A: copy files (keeps the stick's other files)

1. `cargo xtask build`.
2. Format the stick as **FAT32** (any label; Windows: right-click → Format → FAT32).
3. Copy everything inside `out/usb/` to the root of the stick. You get `EFI/BOOT/BOOTX64.EFI`,
   `EFI/BOOT/bedrock.cfg` and `SHA256.txt`.
4. Notes and tasks need a writable data volume: from milestone 5 on, BedrockOS uses a FAT32 volume labelled
   `BRDATA`. With option A, either relabel the stick `BRDATA` or use option B.

## Option B: write the whole disk image (erases the stick)

`out/bedrockos.img` has the boot partition and an empty `BRDATA` data partition.

- Windows: write it with Rufus (DD image mode) or balenaEtcher.
- Linux: `sudo dd if=out/bedrockos.img of=/dev/sdX bs=4M conv=fsync` (check `sdX` with `lsblk` first).
- macOS: `sudo dd if=out/bedrockos.img of=/dev/rdiskN bs=4m` (check with `diskutil list`).

## Boot it

1. Turn **Secure Boot off** in the firmware setup. The BedrockOS image is not signed yet.
2. Use the PC's one-time boot menu (F12, F9, F10 or Esc, depending on the vendor) and pick the stick in
   **UEFI** mode.
3. Check the copy with `SHA256.txt` (`Get-FileHash EFI\BOOT\BOOTX64.EFI` or `sha256sum`).

## What to expect on hardware

Laptop keyboards work through PS/2 emulation; USB keyboards and mice through the xHCI driver. Wired
networking needs an Intel e1000-family or Realtek RTL8168 port, or phone USB tethering. Wi-Fi chips are
**not supported yet** (see `STATUS.md`).
