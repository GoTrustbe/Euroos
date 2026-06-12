#!/usr/bin/env bash
# ============================================================================
#  EuroOS — preview launcher (Linux / macOS)
#  Boots the EuroOS preview in QEMU with networking + a persistent disk.
#  Requires: qemu-system-x86_64  (Linux: apt/dnf install qemu-system-x86 ;
#            macOS: brew install qemu).  Firmware (OVMF.fd) is bundled.
# ============================================================================
set -e
cd "$(dirname "$0")"

IMG="euroos.img"
OVMF="OVMF.fd"
DISK="euroos-disk.qcow2"        # persistent disk, created on first run

command -v qemu-system-x86_64 >/dev/null 2>&1 || {
  echo "QEMU not found. Install it:"
  echo "  Debian/Ubuntu : sudo apt install qemu-system-x86 qemu-utils"
  echo "  Fedora        : sudo dnf install qemu-system-x86 qemu-img"
  echo "  macOS         : brew install qemu"
  exit 1
}

# Hardware acceleration → boots in ~1-2 s. Falls back to software (slower).
ACCEL=""
if [ -e /dev/kvm ]; then
  ACCEL="-accel kvm -cpu host"
elif qemu-system-x86_64 -accel help 2>/dev/null | grep -qi hvf; then
  ACCEL="-accel hvf -cpu host"          # macOS Hypervisor.framework
else
  # Software emulation: expose SMEP/SMAP so EuroOS's kernel hardening (CR4 +
  # per-syscall AC window) is enforced here too (qemu64 hides them by default).
  ACCEL="-cpu qemu64,+smep,+smap"
  echo "(no KVM/HVF acceleration found — running in software emulation, slower)"
fi

# Persistent disk: EuroOS installs itself here on first boot and keeps your files.
[ -f "$DISK" ] || qemu-img create -f qcow2 "$DISK" 512M >/dev/null

echo "Starting EuroOS…  (close the QEMU window to stop; delete $DISK to reset)"
exec qemu-system-x86_64 \
  -machine q35 -m 512M $ACCEL \
  -bios "$OVMF" \
  -drive format=raw,file="$IMG" \
  -drive format=qcow2,file="$DISK",if=none,id=hd0 \
  -device virtio-blk-pci,drive=hd0,disable-modern=on \
  -netdev user,id=n0,ipv4=on -device virtio-net-pci,netdev=n0,disable-modern=on \
  -name "EuroOS preview"
