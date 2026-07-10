#!/usr/bin/env bash
# Boot the EuroKernel image in QEMU with a **real emulated TPM 2.0** (swtpm via
# the QEMU `tpm-tis` device at MMIO 0xFED40000) — the harness that actually
# exercises TPM measured boot + real TPM2 seal/unseal (3D-1). Headless: serial
# is captured to a log and the TPM-relevant `[..]` markers are printed.
#
# Usage: ./scripts/run-swtpm.sh [image] [seconds]
set -euo pipefail
cd "$(dirname "$0")/.."

IMG="${1:-eurokernel.img}"
SECS="${2:-40}"
LOG="serial-swtpm.log"

OVMF=""
for p in /usr/share/ovmf/OVMF.fd /usr/share/OVMF/OVMF.fd /usr/share/edk2-ovmf/x64/OVMF.fd; do
  [ -f "$p" ] && OVMF="$p" && break
done
[ -n "$OVMF" ] || { echo "OVMF not found — install 'ovmf'"; exit 1; }
command -v swtpm >/dev/null || { echo "swtpm not found — install 'swtpm'"; exit 1; }

STATE="$(mktemp -d)"
SOCK="$STATE/swtpm-sock"
cleanup() { kill "${SWTPM_PID:-0}" "${QEMU_PID:-0}" 2>/dev/null || true; rm -rf "$STATE"; }
trap cleanup EXIT

echo "==> starting swtpm (TPM 2.0 emulator) state=$STATE"
swtpm socket --tpm2 --tpmstate dir="$STATE" \
  --ctrl type=unixio,path="$SOCK" \
  --log level=1 &
SWTPM_PID=$!
# Wait for the control socket to appear.
for _ in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.1; done
[ -S "$SOCK" ] || { echo "swtpm socket did not come up"; exit 1; }

ACCEL=(-cpu qemu64,+smep,+smap)
[ -e /dev/kvm ] && ACCEL=(-enable-kvm -cpu host)

echo "==> booting (headless, ${SECS}s) with tpm-tis @ 0xFED40000"
: > "$LOG"
qemu-system-x86_64 \
  -machine q35 -m 256M "${ACCEL[@]}" \
  -bios "$OVMF" \
  -drive "format=raw,file=$IMG" \
  -chardev "socket,id=chrtpm,path=$SOCK" \
  -tpmdev "emulator,id=tpm0,chardev=chrtpm" \
  -device "tpm-tis,tpmdev=tpm0" \
  -display none -serial "file:$LOG" \
  -no-reboot &
QEMU_PID=$!

sleep "$SECS"
kill "$QEMU_PID" 2>/dev/null || true

echo
echo "===== TPM / seal markers from serial ($LOG) ====="
grep -E "\[tpm\]|\[o1\]|\[k3\]|\[u\]|\[af-seal\]|\[3d1\]|\[n2\]|\[3d9\]|\[3d8\]|\[3d7\]|\[3c3\]|\[3d10\]|\[3e1\]|\[q1x\]" "$LOG" || echo "(no TPM markers found)"
