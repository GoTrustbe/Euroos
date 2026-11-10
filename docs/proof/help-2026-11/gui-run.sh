#!/usr/bin/env bash
# T3 (help) GUI run: boot an image with its own QMP/HMP sockets and work dir,
# drive it with scripts/qmp-input.py, leave the screendumps in the work dir.
# usage: t3-gui.sh IMG WORKDIR INPUT_SCRIPT [extra qemu args...]
# env: ACCEL=kvm|tcg (default tcg), QMPIN=path to qmp-input.py, DESKTOP_WAIT (s)
set -u
IMG=$1; W=$2; SCRIPT=$3; shift 3
mkdir -p "$W"
cp "$IMG" "$W/boot.img"
QMP="$W/qmp.sock"; HMP="$W/hmp.sock"; LOG="$W/serial.log"
rm -f "$QMP" "$HMP"; : > "$LOG"
if [ "${ACCEL:-tcg}" = kvm ]; then CPU=(-enable-kvm -cpu host); else CPU=(-cpu qemu64,+smep,+smap); fi
nice -n 10 qemu-system-x86_64 -machine q35 "${CPU[@]}" -m "${MEM:-1G}" -smp "${SMP:-2}" \
  -bios /usr/share/ovmf/OVMF.fd -drive format=raw,file="$W/boot.img" \
  -nic none -display none -serial file:"$LOG" -no-reboot \
  -device qemu-xhci,id=xhci -device usb-kbd,bus=xhci.0 -device usb-tablet,bus=xhci.0 \
  -qmp unix:"$QMP",server,nowait -monitor unix:"$HMP",server,nowait "$@" &
PID=$!
echo "$PID" > "$W/qemu.pid"
END=$((SECONDS + ${DESKTOP_WAIT:-900}))
until grep -aq "interactive loop started" "$LOG"; do
  kill -0 $PID 2>/dev/null || { echo "qemu exited"; exit 1; }
  [ $SECONDS -ge $END ] && { echo "no desktop"; kill $PID; exit 1; }
  sleep 2
done
echo "desktop after ${SECONDS}s"
sleep "${SETTLE:-6}"
( cd "$W" && python3 "${QMPIN:-scripts/qmp-input.py}" "$QMP" "$SCRIPT" 1920 1080 "$HMP" )
sleep 2
kill $PID 2>/dev/null; wait $PID 2>/dev/null
rm -f "$W/boot.img"
echo "done ${SECONDS}s"
