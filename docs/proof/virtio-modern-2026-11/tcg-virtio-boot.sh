#!/usr/bin/env bash
# usage: vboot.sh NAME IMG [extra qemu args...]
# env: MODE=modern|trans|legacy  MEM SMP CONF ("a=b;c=d") TIMEOUT HOLD KEEP NOVEC=1 NOINPUT=1 NONET=1
set -u
NAME=$1 IMG=$2; shift 2
OUT=/root/vt/runs; mkdir -p $OUT/$NAME.d; W=$OUT/$NAME.d; LOG=$OUT/$NAME.log
cp "$IMG" $W/boot.img
if [ -n "${CONF:-}" ]; then printf "%s\n" "$CONF" | tr ';' '\n' > $W/euroos-boot.conf; mcopy -o -i $W/boot.img $W/euroos-boot.conf ::/EFI/BOOT/euroos-boot.conf; fi
rm -f $W/data.raw; truncate -s 64M $W/data.raw
case ${MODE:-modern} in
  modern) P=",disable-legacy=on" ;;
  trans) P="" ;;
  legacy) P=",disable-modern=on" ;;
esac
[ -n "${NOVEC:-}" ] && P="$P,vectors=0"
DEV=(-drive if=none,id=root,format=raw,file=$W/boot.img -device virtio-blk-pci$P,drive=root,bootindex=0
     -drive if=none,id=data,format=raw,file=$W/data.raw -device virtio-blk-pci$P,drive=data)
[ -z "${NONET:-}" ] && DEV+=(-netdev user,id=n0 -device virtio-net-pci$P,netdev=n0)
if [ -z "${NOINPUT:-}" ] && [ "${MODE:-modern}" != legacy ]; then
  IP=""; [ -n "${NOVEC:-}" ] && IP=",vectors=0"
  DEV+=(-device virtio-tablet-pci,disable-legacy=on$IP -device virtio-keyboard-pci,disable-legacy=on$IP)
fi
: > $LOG
rm -f $W/mon.sock $W/qmp.sock
nice -n 10 qemu-system-x86_64 -machine ${MACHINE:-q35} -cpu ${CPU:-qemu64,+smep,+smap} -m ${MEM:-1G} -smp ${SMP:-2} \
  -bios /usr/share/ovmf/OVMF.fd "${DEV[@]}" \
  -vga std -display none -serial file:$LOG -no-reboot \
  -monitor unix:$W/mon.sock,server,nowait -qmp unix:$W/qmp.sock,server,nowait "$@" &
PID=$!; echo $PID > $W/qemu.pid
END=$((SECONDS + ${TIMEOUT:-3600}))
while :; do
  grep -aq "interactive loop started" $LOG && { echo "$NAME: desktop after ${SECONDS}s"; break; }
  kill -0 $PID 2>/dev/null || { echo "$NAME: qemu exited"; break; }
  [ $SECONDS -ge $END ] && { echo "$NAME: timeout"; break; }
  sleep 5
done
sleep ${HOLD:-60}
if [ -n "${KEEP:-}" ]; then echo "kept running pid $PID"; exit 0; fi
kill $PID 2>/dev/null; wait $PID 2>/dev/null
echo "$NAME done"
