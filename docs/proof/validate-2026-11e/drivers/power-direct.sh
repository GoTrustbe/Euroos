#!/bin/bash
# direct power-button repro: boot, wait for the desktop loop, press the ACPI button, report whether QEMU exits and whether the lines arrive
cd /srv/euroos/val-runs/pw/direct; IMG=$1; TAG=$2
LOG=$TAG.serial.log; : > $LOG; rm -f $TAG.qmp
cp --sparse=always $IMG $TAG.raw
taskset -c 4-5 qemu-system-x86_64 -machine q35 -m 512M -smp 1 -enable-kvm -cpu host -bios /usr/share/ovmf/OVMF.fd -drive format=raw,file=$TAG.raw -nic none -display none -serial file:$LOG -qmp unix:$TAG.qmp,server,nowait -no-reboot > $TAG.err 2>&1 < /dev/null &
PID=$!
until grep -aq "interactive loop started" $LOG; do sleep 1; kill -0 $PID 2>/dev/null || break; done
sleep 8
python3 - <<PY
import socket,time
s=socket.socket(socket.AF_UNIX); s.connect("$TAG.qmp"); s.settimeout(5); s.recv(65536)
s.sendall(b"{\"execute\":\"qmp_capabilities\"}\n"); time.sleep(0.2); s.recv(65536)
s.sendall(b"{\"execute\":\"system_powerdown\"}\n"); time.sleep(0.3)
PY
for i in $(seq 1 40); do kill -0 $PID 2>/dev/null || { echo "qemu exited by itself after ~${i}s"; break; }; sleep 1; done
kill -0 $PID 2>/dev/null && { echo "qemu still running after 40 s"; kill $PID; }
echo "lines in serial log: power-button-pressed=$(grep -ac "power button pressed" $LOG) shutting-down=$(grep -ac "shutting down system" $LOG)"
rm -f $TAG.raw
