#!/usr/bin/env bash
# CONTROL EXPERIMENT: does monitor-injected keyboard/mouse input reach the guest AT
# ALL in this qemu setup (-display none)? Boots WITHOUT the chrome pack, waits for the
# desktop loop, injects keys + mouse moves, and reads back the kernel's own IRQ
# counters. If the counters move, injection works and the fault is above the driver;
# if they do not, nothing the X server does could ever have helped.
set -u
cd "$(dirname "$0")/.."
LOG="${1:?usage: input-control.sh /path/to/log}"
mon() { printf '%s\n' "$@" | nc -U -q 1 "$LOG.mon" >/dev/null 2>&1; }
monq() { printf '%s\n' "$1" | nc -U -q 1 "$LOG.mon" 2>&1 | tail -3; }
while pgrep -f qemu-system-x86_64 >/dev/null 2>&1; do sleep 15; done
rm -f "$LOG"
qemu-system-x86_64 -machine q35 -m 3584M -cpu qemu64,+smep,+smap \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file="$PWD/eurokernel.img" \
  -monitor unix:"$LOG.mon",server,nowait \
  -display none -serial stdio -no-reboot > "$LOG" 2>&1 &
Q=$!
START=$(date +%s)
until grep -aq "interactive loop started" "$LOG" 2>/dev/null; do
  kill -0 $Q 2>/dev/null || { echo "qemu died"; exit 1; }
  [ $(( $(date +%s) - START )) -gt 900 ] && { echo "no desktop within 15 min"; break; }
  sleep 5
done
echo "desktop up at $(( $(date +%s) - START ))s"
sleep 20
echo "--- injecting keys + mouse ---"
mon "sendkey a" "sendkey b" "sendkey c"
sleep 5
mon "mouse_move 60 40" "mouse_move 60 40" "mouse_move -30 20"
sleep 5
mon "mouse_button 1"
sleep 2
mon "mouse_button 0"
sleep 25
mon "screendump $LOG.ppm"
sleep 5
echo "--- kernel view ---"
grep -a "keyboard IRQs received\|\[hb\] alive\|desktop\] key\|input kind=" "$LOG" | tail -8
kill $Q 2>/dev/null; wait $Q 2>/dev/null
echo "took $(( $(date +%s) - START ))s, log: $LOG"
