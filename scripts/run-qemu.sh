#!/usr/bin/env bash
# Start the EuroKernel image in QEMU with OVMF (UEFI). With GUI if available,
# otherwise headless. KVM is used if /dev/kvm exists (otherwise TCG).
set -euo pipefail
cd "$(dirname "$0")/.."

IMG="${1:-eurokernel.img}"
OVMF_CANDIDATES=(
  /usr/share/ovmf/OVMF.fd
  /usr/share/OVMF/OVMF.fd
  /usr/share/edk2-ovmf/x64/OVMF.fd
  /opt/homebrew/share/qemu/edk2-x86_64-code.fd
)
OVMF=""
for p in "${OVMF_CANDIDATES[@]}"; do [ -f "$p" ] && OVMF="$p" && break; done
[ -n "$OVMF" ] || { echo "OVMF not found — install 'ovmf'"; exit 1; }

# KVM: -cpu host provides SMEP/SMAP from the real CPU. Without KVM (TCG) a
# qemu64 CPU runs that by default does NOT advertise SMEP/SMAP; enable them explicitly so the
# kernel protection (CR4.SMEP/SMAP + the syscall AC window) is also enforced
# and tested here — TCG emulates both correctly.
ACCEL=(-cpu qemu64,+smep,+smap)
[ -e /dev/kvm ] && ACCEL=(-enable-kvm -cpu host)
DISPLAY_ARG=(-display gtk); [ -z "${DISPLAY:-}" ] && DISPLAY_ARG=(-display none -serial stdio)

echo "==> OVMF: $OVMF  accel: ${ACCEL[*]:-TCG}"
exec qemu-system-x86_64 \
  -machine q35 -m 256M "${ACCEL[@]}" \
  -bios "$OVMF" \
  -drive "format=raw,file=$IMG" \
  "${DISPLAY_ARG[@]}" \
  -no-reboot
