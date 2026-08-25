#!/usr/bin/env bash
# Chromium as a DESKTOP APP: boot to the EuroOS desktop with the Chromium pack
# attached, type `chrome` in the Terminal, and screendump the desktop while the
# browser paints into its framed window. Input goes in over QMP (USB keyboard +
# tablet), the same way a person's does.
set -u
cd "$(dirname "$0")/.."
LOG="${1:?usage: chrome-desktop.sh /path/to/log}"
PACK="${PACK:-/tmp/chrome-pack2.img}"
mon() { printf '%s\n' "$@" | nc -U -q 1 "$LOG.mon" >/dev/null 2>&1; }
# Wait only for MY OWN VMs (the ones reading eurokernel.img, which the build
# rewrites); the persistent eurovnc demo VM never exits and must not block us.
while pgrep -af "qemu-system-x86_64.*eurokernel.img" >/dev/null 2>&1; do sleep 5; done
./scripts/build.sh release >/dev/null 2>&1 || { echo "BUILD FAILED"; exit 1; }
rm -f "$LOG" "$LOG"*.ppm
qemu-system-x86_64 -machine q35 -m 3584M -cpu qemu64,+smep,+smap \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file="$PWD/eurokernel.img" \
  -drive format=raw,file="$PACK",if=virtio \
  -device qemu-xhci,id=xhci -device usb-kbd -device usb-tablet \
  -monitor unix:"$LOG.mon",server,nowait \
  -qmp unix:"$LOG.qmp",server,nowait \
  -display none -serial stdio -no-reboot > "$LOG" 2>&1 &
Q=$!
START=$(date +%s)
until grep -aq "interactive loop started" "$LOG" 2>/dev/null; do
  kill -0 $Q 2>/dev/null || { echo "qemu exited before the desktop"; exit 1; }
  [ $(( $(date +%s) - START )) -gt 1200 ] && { echo "NO DESKTOP within 20 min"; kill $Q; exit 1; }
  sleep 5
done
echo "desktop up at $(( $(date +%s) - START ))s"
sleep 20
mon "screendump $LOG-desktop.ppm"
# Type `chrome` + Enter into the Terminal window (it has focus at boot).
# The qcodes are PHYSICAL keys and this system boots the installer's default layout,
# be-azerty, where the key QEMU calls "m" types a comma and the one it calls
# "semicolon" types the m. Typing the letters as if the guest were US gives `chro,e`.
cat > "$LOG.keys" <<'K'
key c
key h
key r
key o
key semicolon
key e
key ret
K
python3 ./scripts/qmp-input.py "$LOG.qmp" "$LOG.keys" 1920 1080 "$LOG.mon"
echo "typed chrome at $(( $(date +%s) - START ))s"
# Optional interaction phase: DESKTOP_CLICKS names a qmp-input script that runs
# AFTER the t300 sample (chrome has painted by then) — e.g. a click on the page
# link inside the hosted window; the t480/t660 samples then show the outcome.
# Chromium needs minutes under TCG before its first paint: sample the screen.
for t in 120 300 480 660; do
  while [ $(( $(date +%s) - START )) -lt $((t + 60)) ]; do
    kill -0 $Q 2>/dev/null || break 2
    sleep 5
  done
  mon "screendump $LOG-t$t.ppm"
  echo "SHOT $LOG-t$t.ppm at $(( $(date +%s) - START ))s"
  if [ "$t" = 300 ] && [ -n "${DESKTOP_CLICKS:-}" ] && [ -f "$DESKTOP_CLICKS" ]; then
    python3 ./scripts/qmp-input.py "$LOG.qmp" "$DESKTOP_CLICKS" 1920 1080 "$LOG.mon"
    echo "desktop clicks sent at $(( $(date +%s) - START ))s"
  fi
done
kill $Q 2>/dev/null; wait $Q 2>/dev/null
echo "took $(( $(date +%s) - START ))s, log: $LOG"
