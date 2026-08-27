#!/usr/bin/env bash
# UX audit run: boot to the desktop, feed a QMP interaction script (move/click/
# key/shot lines, same grammar as qmp-input.py), no Chromium pack needed.
#   env SCRIPT=/tmp/ux.txt bash scripts/ux-audit.sh /tmp/ux.log
set -u
cd "$(dirname "$0")/.."
LOG="${1:?usage: ux-audit.sh /path/to/log}"
SCRIPT="${SCRIPT:?set SCRIPT=/path/to/interactions.txt}"
mon() { printf '%s\n' "$@" | nc -U -q 1 "$LOG.mon" >/dev/null 2>&1; }
while pgrep -af "qemu-system-x86_64.*eurokernel.img" >/dev/null 2>&1; do sleep 5; done
[ "${SKIP_BUILD:-0}" = "1" ] || ./scripts/build.sh release >/dev/null 2>&1 || { echo "BUILD FAILED"; exit 1; }
rm -f "$LOG"
qemu-system-x86_64 -machine q35 -m 1024M -cpu qemu64,+smep,+smap \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file="$PWD/eurokernel.img" \
  -device qemu-xhci,id=xhci -device usb-kbd -device usb-tablet \
  -monitor unix:"$LOG.mon",server,nowait \
  -qmp unix:"$LOG.qmp",server,nowait \
  -display none -serial stdio -no-reboot > "$LOG" 2>&1 &
Q=$!
START=$(date +%s)
until grep -aq "interactive loop started" "$LOG" 2>/dev/null; do
  kill -0 $Q 2>/dev/null || { echo "qemu died before desktop"; exit 1; }
  [ $(( $(date +%s) - START )) -gt 900 ] && { echo "NO DESKTOP"; kill $Q; exit 1; }
  sleep 5
done
echo "desktop up at $(( $(date +%s) - START ))s"
sleep 15
python3 ./scripts/qmp-input.py "$LOG.qmp" "$SCRIPT" 1920 1080 "$LOG.mon"
echo "interactions done at $(( $(date +%s) - START ))s"
sleep 5
mon "screendump $LOG-final.ppm"
sleep 3
kill $Q 2>/dev/null; wait $Q 2>/dev/null
echo "took $(( $(date +%s) - START ))s, log: $LOG"
