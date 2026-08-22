#!/usr/bin/env bash
# One chrome-UI boot that also INJECTS REAL INPUT. The point is proof, not hope:
#   shot1 = the UI as chrome painted it, before any input
#   then a script of monitor commands (mouse moves, clicks, keys) is fed to the
#   emulated PS/2 devices — the same hardware path a person would use
#   shot2 = the UI after, so the difference is the evidence
# The command script is read from $CLICKS (default /tmp/chrome-click.txt), which may
# appear AFTER shot1 — the run waits for it, so the coordinates can be chosen by
# looking at shot1 instead of guessing them in advance.
set -u
cd "$(dirname "$0")/.."
LOG="${1:?usage: chrome-ui-input.sh /path/to/log}"
CLICKS="${CLICKS:-/tmp/chrome-click.txt}"
PAINT_WAIT="${PAINT_WAIT:-240}"   # seconds after MapWindow before shot1
CLICK_WAIT="${CLICK_WAIT:-600}"   # how long to wait for the command script
AFTER_WAIT="${AFTER_WAIT:-150}"   # seconds after the input before shot2

# One monitor connection per call, so a batch of moves costs one round-trip instead
# of one per step (nc's linger dominates otherwise).
mon() { printf '%s\n' "$@" | nc -U -q 1 "$LOG.mon" >/dev/null 2>&1; }
# A PS/2 mouse is RELATIVE and the device clamps a packet to +-127: move in chunks,
# in small batches so the guest's IRQ handler can drain the 8042 between them.
mrel() {
  local dx=$1 dy=$2 sx sy n=0 batch=()
  while [ "$dx" -ne 0 ] || [ "$dy" -ne 0 ]; do
    sx=$dx; sy=$dy
    [ "$sx" -gt 100 ] && sx=100; [ "$sx" -lt -100 ] && sx=-100
    [ "$sy" -gt 100 ] && sy=100; [ "$sy" -lt -100 ] && sy=-100
    batch+=("mouse_move $sx $sy")
    dx=$((dx - sx)); dy=$((dy - sy)); n=$((n+1))
    if [ ${#batch[@]} -ge 5 ]; then mon "${batch[@]}"; batch=(); fi
  done
  [ ${#batch[@]} -gt 0 ] && mon "${batch[@]}"
  return 0
}
# Absolute: slam into the top-left corner (the driver clamps), then walk out to (x,y).
mabs() { mrel -1200 -1200; mrel "$1" "$2"; }

./scripts/build.sh ${BUILD_PROFILE:-chrome} >/dev/null 2>&1 || { echo "BUILD FAILED"; exit 1; }
while pgrep -x qemu-system-x86 >/dev/null 2>&1 || ps -eo comm | grep -q '^qemu-system-x86$'; do sleep 10; done
rm -f "$LOG" "$LOG.ppm" "$LOG-after.ppm"
qemu-system-x86_64 -machine q35 -m 3584M -cpu qemu64,+smep,+smap \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file="$PWD/eurokernel.img" \
  -drive format=raw,file=${PACK:-/tmp/chrome-pack2.img},if=virtio \
  -device qemu-xhci,id=xhci -device usb-kbd -device usb-tablet \
  -monitor unix:"$LOG.mon",server,nowait \
  -qmp unix:"$LOG.qmp",server,nowait \
  -display none -serial stdio -no-reboot > "$LOG" 2>&1 &
Q=$!
START=$(date +%s); STAGE=map; MAP_AT=0
while kill -0 $Q 2>/dev/null; do
  case $STAGE in
    map)
      if grep -aq "MapWindow id=0x400003" "$LOG" 2>/dev/null; then
        MAP_AT=$(date +%s); STAGE=paint; echo "window mapped at $((MAP_AT-START))s"
      fi ;;
    paint)
      # Painted = the browser window has presented a few times. Waiting a fixed
      # PAINT_WAIT was a guess that cost minutes on every run; the presents are the
      # actual signal, and the timeout is only the fallback.
      PRESENTS=$(grep -ac "present id=0x400003" "$LOG" 2>/dev/null || echo 0)
      if [ "${PRESENTS:-0}" -ge "${PRESENTS_WANTED:-8}" ] || [ $(( $(date +%s) - MAP_AT )) -gt $PAINT_WAIT ]; then
        mon "screendump $LOG.ppm"; echo "SHOT1 $LOG.ppm ($PRESENTS presents, $(( $(date +%s) - MAP_AT ))s after map)"; STAGE=wait; WAIT_AT=$(date +%s)
      fi ;;
    wait)
      if [ -s "$CLICKS" ]; then
        echo "=== INPUT SCRIPT ==="; cat "$CLICKS"
        # Input goes in over QMP to the USB tablet/keyboard: this kernel receives no
        # PS/2 interrupts at all under -display none (measured), while the xHCI HID
        # harvest runs from the timer tick and feeds the very same driver paths.
        python3 ./scripts/qmp-input.py "$LOG.qmp" "$CLICKS" 1920 1080 "$LOG.mon"
        rm -f "$CLICKS"; INPUT_AT=$(date +%s); STAGE=after; echo "=== INPUT SENT ==="
      elif [ $(( $(date +%s) - WAIT_AT )) -gt $CLICK_WAIT ]; then
        echo "no input script within ${CLICK_WAIT}s"; STAGE=done
      fi ;;
    after)
      if [ $(( $(date +%s) - INPUT_AT )) -gt $AFTER_WAIT ]; then
        mon "screendump $LOG-after.ppm"; echo "SHOT2 $LOG-after.ppm"; STAGE=done
      fi ;;
    done) break ;;
  esac
  [ $(( $(date +%s) - START )) -gt 2700 ] && { echo "SAFETY TIMEOUT (45 min)"; break; }
  sleep 5
done
kill $Q 2>/dev/null; wait $Q 2>/dev/null
echo "took $(( $(date +%s) - START ))s, log: $LOG"
