#!/usr/bin/env bash
# One chrome-iteration boot: build, run qemu in the background, kill it the moment
# the guest prints its DONE marker (or the safety timeout hits). Prints the log path.
set -u
cd "$(dirname "$0")/.."
LOG="${1:?usage: chrome-iter.sh /path/to/log}"
./scripts/build.sh release >/dev/null 2>&1 || { echo "BUILD FAILED"; exit 1; }
while pgrep -x qemu-system-x86 >/dev/null 2>&1 || ps -eo comm | grep -q '^qemu-system-x86$'; do sleep 10; done
rm -f "$LOG"
qemu-system-x86_64 -machine q35 -m 3584M -cpu qemu64,+smep,+smap \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file="$PWD/eurokernel.img" \
  -drive format=raw,file=${PACK:-/tmp/hs-pack.img},if=virtio \
  -display none -serial stdio -no-reboot > "$LOG" 2>&1 &
Q=$!
START=$(date +%s)
LAST_SIZE=0; LAST_GROW=$START
while kill -0 $Q 2>/dev/null; do
  grep -aq "chrome-run\] DONE" "$LOG" 2>/dev/null && break
  NOW=$(date +%s)
  # No stall detection: a chrome thread woken for real work can compute for many
  # wall minutes under TCG without printing a line, and every heartbeat scheme so
  # far (guest ticks, iterations, RTC) went quiet in exactly that case. A healthy
  # slow run deserves its full budget; the hard cap below bounds the waste.
  [ $(( NOW - START )) -gt 1500 ] && { echo "SAFETY TIMEOUT (25 min)"; break; }
  sleep 10
done
kill $Q 2>/dev/null; wait $Q 2>/dev/null
echo "took $(( $(date +%s) - START ))s, log: $LOG"
