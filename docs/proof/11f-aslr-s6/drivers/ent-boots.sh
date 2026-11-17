#!/usr/bin/env bash
# Short boots of the self-test image for the entropy and [linux-rand] lines
# (after eboots.sh of proof/integ-2026-11b). Each boot runs until the marker
# (default: the [linux-rand] verdict, which comes after [3d8], [3d7] and [3d3])
# is on the serial log or the timeout passes; then this QEMU, and only this
# one, is stopped by its PID. No NIC.
# usage: short-boots.sh IMG OUTDIR TAG N PINS ACCEL("kvm"|"tcg") CPU [MARKER] [TIMEOUT] [SMP]
set -u
IMG=$1 OUT=$2 TAG=$3 N=$4 PINS=$5 ACCEL=$6 CPU=$7
MARK=${8:-"[linux-rand] getrandom"} TMO=${9:-300} SMP=${10:-4}
OVMF=/usr/share/ovmf/OVMF.fd
D=$OUT/$TAG; mkdir -p "$D"
ACC=(-cpu "$CPU"); [ "$ACCEL" = kvm ] && ACC=(-enable-kvm -cpu "$CPU")
for i in $(seq 1 "$N"); do
  [ -f "$OUT/STOP-SHORT" ] && break
  LOG=$D/boot-$i.log; : > "$LOG"
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
  es=$(grep -ac "^\[entropy\] CSPRNG seeded" "$LOG"); en=$(grep -ac "^\[entropy\] CSPRNG NOT SEEDED" "$LOG"); el=$(grep -ac "^\[entropy\]" "$LOG")
  lr=$(grep -a "^\[linux-rand\] getrandom" "$LOG" | grep -c "OK ✓"); lf=$(grep -a "^\[linux-rand\] getrandom" "$LOG" | grep -c "FAILED")
  fl=$(grep -a "^\[[a-z0-9_./-]*\]" "$LOG" | grep -c "FAILED")
  cr=$(grep -aEc "KERNEL PANIC|GENERAL PROTECTION|\[idt\] PAGE FAULT|DOUBLE FAULT|panicked at" "$LOG")
  ent=$(grep -a "^\[entropy\]" "$LOG" | tr -d '\r' | head -1 | cut -c1-260)
  echo "boot=$i end=$why secs=$(echo "$T1 - $T0" | bc) 3d8 ok=$o8 failed=$f8 | 3d7 ok=$o7 failed=$f7 | 3d3 ok=$o3 failed=$f3 | entropy lines=$el seeded=$es not-seeded=$en | linux-rand ok=$lr failed=$lf | failed-lines=$fl crash-lines=$cr | $ent" >> "$D/summary.txt"
  grep -a "^\[3d8\]\|^\[3d7\]\|^\[3d3\]\|^\[entropy\]\|^\[tpm\]\|^\[linux-rand\] getrandom\|^\[linux-rand\] .*randprobe: at_random=\|^\[linux-rand\] .*randprobe: stack_guard=\|^\[linux-rand\] .*randprobe: getrandom=\|^\[linux-rand\] .*randprobe: urandom=\|^\[linux-rand\] .*randprobe: random=\|^\[linux-rand\] .*randprobe: stat " "$LOG" | tr -d '\r' > "$D/lines-$i.txt"
  grep -a "^\[[a-z0-9_./-]*\]" "$LOG" | grep "FAILED" | tr -d '\r' | cut -c1-240 > "$D/failed-$i.txt"; [ -s "$D/failed-$i.txt" ] || rm -f "$D/failed-$i.txt"
  grep -a "^\[aslr\|^\[arena-" "$LOG" | tr -d "\r" > "$D/aslr-$i.txt"
  [ "$why" = marker ] && [ "$fl$cr" = 00 ] && rm -f "$LOG"
  rm -f "$D/qemu-$i.err"
done
echo "done $(date +%T)" >> "$D/summary.txt"
