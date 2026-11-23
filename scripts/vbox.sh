#!/usr/bin/env sh
# One-command VirtualBox runner for BedrockOS (Linux / macOS host).
#   scripts/vbox.sh [start|create|stop|delete|screenshot] [--headless] [--res 1920x1080] [--net nat|bridged|intnet|none]
#                   [--bridge <adapter>] [--build]
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/out"
VM=BedrockOS
VDI="$OUT/bedrockos.vdi"
SERIAL="$OUT/serial.log"
ACTION=start RES=1280x800 TYPE=gui NET=nat BRIDGE="" BUILD=0
while [ $# -gt 0 ]; do
  case "$1" in
    start|create|stop|delete|screenshot) ACTION=$1 ;;
    --headless) TYPE=headless ;;
    --res) RES=$2; shift ;;
    --net) NET=$2; shift ;;
    --bridge) BRIDGE=$2; shift ;;
    --build) BUILD=1 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done
VBM=$(command -v VBoxManage || true)
[ -z "$VBM" ] && [ -x /Applications/VirtualBox.app/Contents/MacOS/VBoxManage ] && VBM=/Applications/VirtualBox.app/Contents/MacOS/VBoxManage
[ -z "$VBM" ] && { echo "VBoxManage not found: install VirtualBox 7.x" >&2; exit 1; }
exists()  { "$VBM" list vms | grep -q "^\"$VM\" "; }
running() { "$VBM" list runningvms | grep -q "^\"$VM\" "; }

case "$ACTION" in
  stop) running && "$VBM" controlvm "$VM" poweroff; exit 0 ;;
  delete) running && { "$VBM" controlvm "$VM" poweroff; sleep 2; }; exists && "$VBM" unregistervm "$VM" --delete-all; exit 0 ;;
  screenshot) f="$OUT/shot-$(date +%Y%m%d-%H%M%S).png"; "$VBM" controlvm "$VM" screenshotpng "$f"; echo "$f"; exit 0 ;;
esac

if [ "$BUILD" = 1 ] || [ ! -f "$VDI" ]; then (cd "$ROOT" && cargo xtask build); fi
running && { "$VBM" controlvm "$VM" poweroff; sleep 2; }
if ! exists; then
  "$VBM" createvm --name "$VM" --ostype Other_64 --register --basefolder "$OUT/vm"
  "$VBM" modifyvm "$VM" --firmware efi64 --memory 1024 --cpus 2 --ioapic on --rtcuseutc on \
    --graphicscontroller vmsvga --vram 128 --audio-driver none \
    --usbxhci on --mouse usbtablet --keyboard ps2 \
    --uart1 0x3F8 4 --uart-mode1 file "$SERIAL" \
    --boot1 disk --boot2 none --boot3 none --boot4 none
  "$VBM" storagectl "$VM" --name SATA --add sata --controller IntelAhci --portcount 2
fi
# USB tablet: absolute positions, so the host pointer moves in and out of the VM window freely (mouse
# integration); BedrockOS reads its reports. Also updates VMs made by older scripts.
"$VBM" modifyvm "$VM" --mouse usbtablet
case "$NET" in
  nat) "$VBM" modifyvm "$VM" --nic1 nat --nictype1 82540EM --cableconnected1 on ;;
  bridged) [ -n "$BRIDGE" ] || { echo "--net bridged needs --bridge <adapter> (VBoxManage list bridgedifs)" >&2; exit 2; }
           "$VBM" modifyvm "$VM" --nic1 bridged --bridgeadapter1 "$BRIDGE" --nictype1 82540EM --cableconnected1 on ;;
  intnet) "$VBM" modifyvm "$VM" --nic1 intnet --intnet1 bedrock-wifi-sim --nictype1 82540EM --cableconnected1 on ;;
  none) "$VBM" modifyvm "$VM" --nic1 none ;;
esac
"$VBM" setextradata "$VM" VBoxInternal2/EfiGraphicsResolution "$RES"
"$VBM" storageattach "$VM" --storagectl SATA --port 0 --device 0 --medium none 2>/dev/null || true
"$VBM" closemedium disk "$VDI" 2>/dev/null || true
"$VBM" storageattach "$VM" --storagectl SATA --port 0 --device 0 --type hdd --medium "$VDI"
[ "$ACTION" = create ] && { echo "VM '$VM' ready."; exit 0; }
rm -f "$SERIAL"
"$VBM" startvm "$VM" --type "$TYPE"
echo "Started '$VM' ($RES, network $NET). Serial log: $SERIAL"
