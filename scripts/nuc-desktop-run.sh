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
MEM="${MEM:-4608M}"
for f in "$IMG" "$PACK" "$NSSPACK" "$OVMF"; do
  [ -f "$f" ] || { echo "missing: $f"; exit 1; }
done
[ -e /dev/kvm ] || { echo "no /dev/kvm - this script is for the NUC; use chrome-desktop.sh under TCG"; exit 1; }
mon() { printf '%s\n' "$@" | nc -U -q 1 "$LOG.mon" >/dev/null 2>&1; }

# One VM at a time on this image: a second qemu cannot take the write lock and
# dies before the desktop ("Failed to get \"write\" lock"). Wait for the previous
# run to finish rather than fail. pkill -x, never -f: -f matches the ssh command
# line that started us.
while pgrep -x qemu-system-x86 >/dev/null 2>&1; do sleep 5; done
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

# CLICK_AT="X Y [X2 Y2 ...]" clicks those absolute screen points after the first
# sample. Chrome can come up on a modal it will sit on forever (the profile-error
# dialog); a click on its button is how you find out whether that dialog is the
# wall or just noise in front of it. The tablet is absolute, so a point read off a
# screendump lands where the screendump said.
# Wedge watchdog. One run in six goes silent right after the keystrokes with the
# IRQs still firing (runs 3 and 12). When the serial log has not grown for 60 s,
# inject an NMI: it fires under IF=0 and the kernel's probe prints the RIP and a
# task census, which no ordinary log line can reach. Twice, 4 s apart, then the
# run continues so the screendumps still say what the screen looked like.
LASTLINES=0; QUIET=0; NMI_DONE=0
watchdog() {
  local n; n=$(wc -l < "$LOG")
  if [ "$n" = "$LASTLINES" ]; then QUIET=$((QUIET + 5)); else QUIET=0; fi
  LASTLINES=$n
  if [ $QUIET -ge 60 ] && [ $NMI_DONE = 0 ]; then
    NMI_DONE=1
    echo "WEDGE: no serial output for ${QUIET}s at $(( $(date +%s) - START ))s, injecting NMI"
    mon "nmi"; sleep 4; mon "nmi"; sleep 4
    grep -a -A 40 "NMI PROBE" "$LOG" | head -60
  fi
}
for t in ${SAMPLES:-120 300 480 660}; do
  while [ $(( $(date +%s) - START )) -lt $t ]; do
    kill -0 $Q 2>/dev/null || break 2
    sleep 5
    watchdog
  done
  mon "screendump $LOG-t$t.ppm"
  echo "SHOT $LOG-t$t.ppm at $(( $(date +%s) - START ))s"
  if [ -n "${CLICK_AT:-}" ] && [ "$t" = "${CLICK_AFTER:-120}" ]; then
    : > "$LOG.clicks"
    set -- ${CLICK_AT}
    while [ $# -ge 2 ]; do
      printf 'move %s %s\nwait 1\nclick\nwait 1\n' "$1" "$2" >> "$LOG.clicks"
      shift 2
    done
    python3 "$DIR/qmp-input.py" "$LOG.qmp" "$LOG.clicks" 1920 1080 "$LOG.mon"
    echo "clicked $CLICK_AT at $(( $(date +%s) - START ))s"
  fi
  # KEYS_AFTER="ret esc ..." types physical keys (qcodes) at the sample KEYS_AT
  # (default: the click's sample, right after the clicks). A dialog's default
  # button answers Enter, which tells a click that is not arriving apart from a
  # dialog that is not listening; with KEYS_AT one sample later, the screendump
  # in between says whether the click alone was enough.
  if [ -n "${KEYS_AFTER:-}" ] && [ "$t" = "${KEYS_AT:-${CLICK_AFTER:-120}}" ]; then
    sleep 3
    : > "$LOG.keys2"
    for k in $KEYS_AFTER; do printf 'key %s\nwait 1\n' "$k" >> "$LOG.keys2"; done
    python3 "$DIR/qmp-input.py" "$LOG.qmp" "$LOG.keys2" 1920 1080 "$LOG.mon"
    echo "keys $KEYS_AFTER at $(( $(date +%s) - START ))s"
  fi
done
kill $Q 2>/dev/null; wait $Q 2>/dev/null
echo "took $(( $(date +%s) - START ))s, log: $LOG"
