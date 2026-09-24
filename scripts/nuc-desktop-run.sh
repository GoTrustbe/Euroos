#!/usr/bin/env bash
# ============================================================================
#  nuc-desktop-run.sh — run the DESKTOP chrome scenario on the lab NUC (KVM).
#
#  The TCG version of this (chrome-desktop.sh) builds locally and ties the guest
#  clock to executed instructions. On the NUC there is real virtualisation, so
#  this one takes a prebuilt image, enables KVM and drops -icount: under KVM the
#  guest keeps real time by itself, and icount is not compatible with it anyway.
#
#  Lives in the repo ON PURPOSE. Its predecessor existed only in /root on a
#  SystemRescue live system, which is a tmpfs, so a power cut erased it.
#
#  Usage (on the NUC, as root):
#     scripts/nuc-desktop-run.sh /root/euroos/run1.log
#  Env:
#     IMG      kernel image            (default /root/euroos/eurokernel.img)
#     PACK     chromium EuroPack       (default /root/euroos/chrome-pack2.img)
#     NSSPACK  NSS EuroPack, for https (default /root/euroos/nss-pack.img)
#     SAMPLES  screendump times in seconds after boot (default "120 300 480 660")
#     MEM      guest memory            (default 3584M)
# ============================================================================
set -u
LOG="${1:?usage: nuc-desktop-run.sh /path/to/log}"
DIR="$(cd "$(dirname "$0")" && pwd)"
IMG="${IMG:-/root/euroos/eurokernel.img}"
PACK="${PACK:-/root/euroos/chrome-pack2.img}"
NSSPACK="${NSSPACK:-/root/euroos/nss-pack.img}"
OVMF="${OVMF:-/usr/share/edk2/x64/OVMF.4m.fd}"
MEM="${MEM:-3584M}"
for f in "$IMG" "$PACK" "$NSSPACK" "$OVMF"; do
  [ -f "$f" ] || { echo "missing: $f"; exit 1; }
done
[ -e /dev/kvm ] || { echo "no /dev/kvm - this script is for the NUC; use chrome-desktop.sh under TCG"; exit 1; }
mon() { printf '%s\n' "$@" | nc -U -q 1 "$LOG.mon" >/dev/null 2>&1; }

rm -f "$LOG" "$LOG"*.ppm "$LOG.mon" "$LOG.qmp"
# Both packs are attached: the kernel scans every disk for a EuroPack volume, and
# https needs the NSS one (chrome loads its software token and trust roots as
# separate .so files, outside the library closure a linker reports).
qemu-system-x86_64 -machine q35 -enable-kvm -cpu host -m "$MEM" \
  -bios "$OVMF" \
  -drive format=raw,file="$IMG" \
  -drive format=raw,file="$PACK",if=virtio \
  -drive format=raw,file="$NSSPACK",if=virtio \
  -device qemu-xhci,id=xhci -device usb-kbd -device usb-tablet \
  -netdev user,id=n0 -device virtio-net-pci,netdev=n0 \
  -monitor unix:"$LOG.mon",server,nowait \
  -qmp unix:"$LOG.qmp",server,nowait \
  -display none -serial stdio -no-reboot > "$LOG" 2>&1 &
Q=$!
START=$(date +%s)
until grep -aq "interactive loop started" "$LOG" 2>/dev/null; do
  kill -0 $Q 2>/dev/null || { echo "qemu exited before the desktop"; tail -20 "$LOG"; exit 1; }
  [ $(( $(date +%s) - START )) -gt 600 ] && { echo "NO DESKTOP within 10 min"; kill $Q; exit 1; }
  sleep 2
done
echo "desktop up at $(( $(date +%s) - START ))s"
sleep 15
mon "screendump $LOG-desktop.ppm"

# Type `chrome` + Enter into the Terminal window (it has focus at boot).
# The qcodes are PHYSICAL keys and this system boots be-azerty, where the key
# QEMU calls "semicolon" types an m. Sending the letters as if the guest were
# US-layout gives `chro,e`.
cat > "$LOG.keys" <<'K'
key c
key h
key r
key o
key semicolon
key e
key ret
K
python3 "$DIR/qmp-input.py" "$LOG.qmp" "$LOG.keys" 1920 1080 "$LOG.mon"
echo "typed chrome at $(( $(date +%s) - START ))s"

for t in ${SAMPLES:-120 300 480 660}; do
  while [ $(( $(date +%s) - START )) -lt $t ]; do
    kill -0 $Q 2>/dev/null || break 2
    sleep 5
  done
  mon "screendump $LOG-t$t.ppm"
  echo "SHOT $LOG-t$t.ppm at $(( $(date +%s) - START ))s"
done
kill $Q 2>/dev/null; wait $Q 2>/dev/null
echo "took $(( $(date +%s) - START ))s, log: $LOG"
