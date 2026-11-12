#!/usr/bin/env bash
# Short boots for the entropy lines (BUG-065). Each boot runs until the marker
# (default "[3d3]") is on the serial log or the timeout passes; then this QEMU,
# and only this one, is stopped by its PID.
# usage: eboots.sh IMG OUTDIR TAG N PINS ACCEL("kvm"|"tcg") CPU [MARKER] [TIMEOUT] [SMP]
set -u
IMG=$1 OUT=$2 TAG=$3 N=$4 PINS=$5 ACCEL=$6 CPU=$7
MARK=${8:-"[3d3]"} TMO=${9:-240} SMP=${10:-4}
OVMF=/usr/share/ovmf/OVMF.fd
D=$OUT/$TAG; mkdir -p "$D"
ACC=(-cpu "$CPU"); [ "$ACCEL" = kvm ] && ACC=(-enable-kvm -cpu "$CPU")
for i in $(seq 1 "$N"); do
  [ -f "$OUT/STOP" ] && break
  LOG=$D/boot-$i.log; : > "$LOG"
  FREQ=""
  if [ -r /sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq ]; then
    for c in $(echo "$PINS" | awk -F- '{for(i=$1;i<=$2;i++)print i}'); do FREQ="$FREQ $(cat /sys/devices/system/cpu/cpu$c/cpufreq/scaling_cur_freq 2>/dev/null)"; done
  fi
  T0=$(date +%s.%N)
  TMPDIR=$D taskset -c "$PINS" qemu-system-x86_64 -machine q35 "${ACC[@]}" -m 2G -smp "$SMP" -bios $OVMF \
    -drive format=raw,file="$IMG",snapshot=on -nic none -display none -serial file:"$LOG" -no-reboot \
    > "$D/qemu-$i.err" 2>&1 < /dev/null &
  PID=$!
  why=marker
  END=$((SECONDS + TMO))
  while ! grep -aqF "$MARK" "$LOG"; do
    kill -0 "$PID" 2>/dev/null || { why=exit; break; }
    [ $SECONDS -ge $END ] && { why=timeout; break; }
    sleep 0.2
  done
  T1=$(date +%s.%N)
  sleep 0.3
  if [ -n "$PID" ] && [ "$PID" -gt 1 ] 2>/dev/null; then kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; fi
  f8=$(grep -a "^\[3d8\]" "$LOG" | grep -c FAILED); f7=$(grep -a "^\[3d7\]" "$LOG" | grep -c FAILED); f3=$(grep -a "^\[3d3\]" "$LOG" | grep -c FAILED)
  o8=$(grep -a "^\[3d8\]" "$LOG" | grep -c "OK"); o7=$(grep -a "^\[3d7\]" "$LOG" | grep -c "OK"); o3=$(grep -a "^\[3d3\]" "$LOG" | grep -c "OK")
  ent=$(grep -a "^\[entropy\]" "$LOG" | tr -d '\r' | head -1)
  echo "boot=$i end=$why secs=$(echo "$T1 - $T0" | bc) 3d8 ok=$o8 failed=$f8 | 3d7 ok=$o7 failed=$f7 | 3d3 ok=$o3 failed=$f3 | hostfreq=$FREQ | $ent" >> "$D/summary.txt"
  grep -a "^\[3d8\]\|^\[3d7\]\|^\[3d3\]\|^\[entropy\]\|^\[ent-diag\]\|^\[boot-time\]" "$LOG" | tr -d '\r' > "$D/lines-$i.txt"
  [ "$why" = marker ] && [ "$f8$f7$f3" = 000 ] && rm -f "$LOG"
  rm -f "$D/qemu-$i.err"
done
echo "done $(date +%T)" >> "$D/summary.txt"
