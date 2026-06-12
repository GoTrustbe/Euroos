#!/usr/bin/env bash
# Start de EuroKernel-image in QEMU met OVMF (UEFI). Met GUI indien beschikbaar,
# anders headless. KVM wordt gebruikt als /dev/kvm bestaat (anders TCG).
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
[ -n "$OVMF" ] || { echo "OVMF niet gevonden — installeer 'ovmf'"; exit 1; }

# KVM: -cpu host levert SMEP/SMAP van de echte CPU. Zonder KVM (TCG) draait een
# qemu64-CPU die SMEP/SMAP standaard NIET adverteert; expliciet aanzetten zodat de
# kernel-bescherming (CR4.SMEP/SMAP + het syscall-AC-venster) ook hier afgedwongen
# en getest wordt — TCG emuleert beide correct.
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
