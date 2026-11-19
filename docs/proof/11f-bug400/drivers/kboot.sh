#!/usr/bin/env bash
# One KVM boot of a self-test image on the R630 until MARKER is on the serial
# log (or the timeout passes); then this QEMU, and only this one, is stopped by
# its PID. No NIC. usage: kboot.sh IMG OUTDIR NAME MEM SMP PINS [MARKER] [TIMEOUT]
set -u
IMG=$1 OUT=$2 NAME=$3 MEM=$4 SMP=$5 PINS=$6
MARK=${7:-"interactive loop started"} TMO=${8:-600}
mkdir -p "$OUT"
LOG=$OUT/$NAME.log; : > "$LOG"
TMPDIR=$OUT taskset -c "$PINS" qemu-system-x86_64 -machine q35 ${ACCEL:--enable-kvm -cpu host} -m "$MEM" -smp "$SMP" \
  -bios /usr/share/ovmf/OVMF.fd -drive format=raw,file="$IMG",snapshot=on -nic none -display none -audiodev wav,id=a0,path=$OUT/$NAME.wav -device intel-hda -device hda-duplex,audiodev=a0 \
  -serial file:"$LOG" -no-reboot > "$OUT/$NAME.qemu.err" 2>&1 < /dev/null &
PID=$!
echo "$PID" > "$OUT/$NAME.pid"
why=marker
END=$((SECONDS + TMO))
while ! grep -aqF "$MARK" "$LOG"; do
  kill -0 "$PID" 2>/dev/null || { why=exit; break; }
  [ $SECONDS -ge $END ] && { why=timeout; break; }
  sleep 0.5
done
sleep ${AFTER:-2}
if [ -n "$PID" ] && [ "$PID" -gt 1 ] 2>/dev/null; then kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; fi
rm -f "$OUT/$NAME.pid"
echo "$NAME end=$why after ${SECONDS}s failed-lines=$(grep -a '^\[[a-z0-9_./-]*\]' "$LOG" | grep -c FAILED) crash-lines=$(grep -aEc 'KERNEL PANIC|GENERAL PROTECTION|\[idt\] PAGE FAULT|DOUBLE FAULT|panicked at' "$LOG")"
