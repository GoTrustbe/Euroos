#!/usr/bin/env bash
# One KVM boot on the R630 with `-cpu host` (46 physical address bits: OVMF puts
# the 64-bit PCI window at 56 TiB), no network, own work directory.
# usage: kvm-boot.sh NAME IMG OUTDIR [extra qemu args...]
# env: PIN (cpu list) MEM SMP TIMEOUT HOLD
set -u
NAME=$1 IMG=$2 OUT=$3; shift 3
mkdir -p "$OUT/$NAME.d"; W="$OUT/$NAME.d"; LOG="$OUT/$NAME.log"
cp --sparse=always "$IMG" "$W/boot.img"
rm -f "$W/nvme.raw"; truncate -s 64M "$W/nvme.raw"
ARGS=()
for a in "$@"; do ARGS+=("${a//@W@/$W}"); done
: > "$LOG"
taskset -c "${PIN:-26-29}" qemu-system-x86_64 -machine q35 -enable-kvm -cpu host -m "${MEM:-4G}" -smp "${SMP:-4}" \
  -bios /usr/share/ovmf/OVMF.fd -drive format=raw,file="$W/boot.img" -nic none \
  -display none -serial file:"$LOG" -no-reboot -monitor unix:"$W/mon.sock",server,nowait "${ARGS[@]}" &
PID=$!; echo $PID > "$W/qemu.pid"
END=$((SECONDS + ${TIMEOUT:-600})); why=""
while :; do
  grep -aq "interactive loop started" "$LOG" && { echo "$NAME: desktop after ${SECONDS}s"; break; }
  grep -aqE "KERNEL PANIC|\[idt\] PAGE FAULT|DOUBLE FAULT|GENERAL PROTECTION FAULT" "$LOG" && { why=crash; echo "$NAME: CRASH after ${SECONDS}s"; break; }
  kill -0 $PID 2>/dev/null || { why=exit; echo "$NAME: qemu exited"; break; }
  [ $SECONDS -ge $END ] && { why=timeout; echo "$NAME: timeout"; break; }
  sleep 2
done
sleep "${HOLD:-20}"
kill $PID 2>/dev/null; wait $PID 2>/dev/null
rm -f "$W/boot.img" "$W/nvme.raw"
