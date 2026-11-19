#!/usr/bin/env bash
# One KVM boot of a self-test image until MARKER is on the serial log, the guest
# is silent for SILENT s, or TIMEOUT passes. A silent or timed-out guest gets an
# NMI (probe on the serial log) before it is stopped. Only this QEMU is stopped,
# by its PID. No NIC. usage: kboot.sh IMG OUTDIR NAME MEM SMP PINS [MARKER] [TIMEOUT] [SILENT]
set -u
IMG=$1 OUT=$2 NAME=$3 MEM=$4 SMP=$5 PINS=$6
MARK=${7:-"interactive loop started"} TMO=${8:-900} SIL=${9:-120}
mkdir -p "$OUT"
LOG=$OUT/$NAME.log; : > "$LOG"; MON=$OUT/$NAME.mon
TMPDIR=$OUT taskset -c "$PINS" qemu-system-x86_64 -machine q35 -enable-kvm -cpu host -m "$MEM" -smp "$SMP" \
  -bios /usr/share/ovmf/OVMF.fd -drive format=raw,file="$IMG",snapshot=on -nic none -display none \
  -audiodev none,id=a0 -device intel-hda -device hda-duplex,audiodev=a0 \
  -monitor unix:$MON,server,nowait -serial file:"$LOG" -no-reboot > "$OUT/$NAME.qemu.err" 2>&1 < /dev/null &
PID=$!
why=marker; END=$((SECONDS + TMO)); last=0; lastt=$SECONDS
while ! grep -aqF "$MARK" "$LOG"; do
  kill -0 "$PID" 2>/dev/null || { why=exit; break; }
  sz=$(stat -c %s "$LOG"); if [ "$sz" != "$last" ]; then last=$sz; lastt=$SECONDS; fi
  [ $((SECONDS - lastt)) -ge $SIL ] && { why=silent; break; }
  [ $SECONDS -ge $END ] && { why=timeout; break; }
  sleep 1
done
if [ "$why" = silent ] || [ "$why" = timeout ]; then
  python3 -c "import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall(b\"nmi\n\")" "$MON" 2>/dev/null
  sleep 8
fi
sleep ${AFTER:-2}
if [ -n "$PID" ] && [ "$PID" -gt 1 ] 2>/dev/null; then kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; fi
echo "$NAME end=$why after ${SECONDS}s swapirq=$(grep -a "^\[swap-irq\]" "$LOG" | tail -1 | grep -o "OK ✓\|FAILED\|SKIPPED" | head -1) failed-lines=$(grep -a "^\[[a-z0-9_./-]*\]" "$LOG" | grep -c FAILED) crash-lines=$(grep -aEc "KERNEL PANIC|GENERAL PROTECTION|\[idt\] PAGE FAULT|DOUBLE FAULT|panicked at|WATCHDOG TRIPPED" "$LOG") nmi=$(grep -ac "NMI PROBE" "$LOG")"
