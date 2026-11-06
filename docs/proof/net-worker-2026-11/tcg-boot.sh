#!/usr/bin/env bash
# One TCG boot as in docs/proof/integrate-loadtest-2026-11-03 (integrate-net-mem).
# usage: boot.sh NAME MEM SMP IOMMU(0|1) KIND(user|kernel|storage|combined) IMG OUTDIR
# A boot without desktop, or whose serial log is silent SILENT s, gets an NMI
# through the QEMU monitor (the kernel prints its NMI probe) before it is stopped.
set -u
NAME=$1 MEM=$2 SMP=$3 IOMMU=$4 KIND=$5 IMG=$6 OUT=$7
mkdir -p "$OUT/$NAME.d"
W="$OUT/$NAME.d"
LOG="$OUT/$NAME.log"
cp "$IMG" "$W/boot.img"
CONF=""
[ "$KIND" = kernel ] && CONF="net.driver=kernel"
[ "$KIND" = storage ] || [ "$KIND" = combined ] && CONF="storage.nvme=user"
if [ -n "$CONF" ]; then
  printf "%s\n" "$CONF" > "$W/euroos-boot.conf"
  mcopy -o -i "$W/boot.img" "$W/euroos-boot.conf" ::/EFI/BOOT/euroos-boot.conf
fi
EXTRA=()
[ "$IOMMU" = 1 ] && EXTRA+=(-device intel-iommu,intremap=off)
if [ "$KIND" = storage ] || [ "$KIND" = combined ]; then
  rm -f "$W/nvme.raw"; truncate -s 64M "$W/nvme.raw"
  EXTRA+=(-drive if=none,id=nv0,file="$W/nvme.raw",format=raw -device nvme,serial=EUROD1,drive=nv0)
fi
if [ "$KIND" = combined ]; then
  rm -f "$W/ld0.raw" "$W/ld1.raw"; truncate -s 64M "$W/ld0.raw"; truncate -s 256M "$W/ld1.raw"
  EXTRA+=(-fw_cfg name=opt/ovmf/X-PciMmio64Mb,string=0 -device megasas-gen2,id=mr0
    -drive if=none,id=ld0,file="$W/ld0.raw",format=raw -device scsi-hd,bus=mr0.0,drive=ld0,scsi-id=0
    -drive if=none,id=ld1,file="$W/ld1.raw",format=raw -device scsi-hd,bus=mr0.0,drive=ld1,scsi-id=1)
fi
: > "$LOG"
MON="$W/monitor.sock"; rm -f "$MON"
taskset -c ${PIN:-0-15} nice -n 10 qemu-system-x86_64 -machine q35 -cpu qemu64,+smep,+smap -m "$MEM" -smp "$SMP" \
  -bios /usr/share/ovmf/OVMF.fd -drive format=raw,file="$W/boot.img" "${EXTRA[@]}" \
  -nic user,model=e1000e -display none -serial file:"$LOG" -no-reboot \
  -monitor unix:"$MON",server,nowait &
PID=$!
echo "$PID" > "$W/qemu.pid"
END=$((SECONDS + ${TIMEOUT:-5400}))
SILENT=${SILENT:-900}
last=0; since=$SECONDS; why=""
while :; do
  grep -aq "interactive loop started" "$LOG" && break
  kill -0 $PID 2>/dev/null || { why="exit"; break; }
  [ $SECONDS -ge $END ] && { why="timeout"; break; }
  sz=$(stat -c%s "$LOG"); if [ "$sz" != "$last" ]; then last=$sz; since=$SECONDS; fi
  [ $((SECONDS - since)) -ge $SILENT ] && { why="silent ${SILENT}s"; break; }
  sleep 5
done
if [ -z "$why" ]; then sleep 120
else
  echo "$NAME: no desktop ($why) $(date +%T)" >> "$OUT/problems.txt"
  if kill -0 $PID 2>/dev/null; then
    python3 -c "import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall(b\"nmi\n\")" "$MON" && echo "$NAME: NMI sent" >> "$OUT/problems.txt"
    sleep 30
  fi
fi
kill $PID 2>/dev/null; wait $PID 2>/dev/null
rm -f "$W/boot.img" "$W/nvme.raw" "$W/ld0.raw" "$W/ld1.raw"
echo "$NAME done $(date +%T) after ${SECONDS}s"
