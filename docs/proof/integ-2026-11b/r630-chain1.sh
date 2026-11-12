#!/bin/bash
# R630 (KVM), CPUs 28-31: NVMe untouched check, jitter-only entropy boots, screenshots of the public image.
# QEMU only with -nic none or restrict=on; every QEMU is stopped by its own PID (timeout, or the harness).
set -u
B=/srv/euroos/integ2-runs; S=$B/s/scripts; U=$B/s/docs/proof/uidebug-2026-11
cd $B

# 1. The public image boots beside a 64 MiB NVMe disk full of random data: the disk is not written.
W=$B/nvme-check; rm -rf $W; mkdir -p $W
cp --sparse=always $B/pub5.img $W/img.raw
dd if=/dev/urandom of=$W/nv.raw bs=1M count=64 status=none
H1=$(sha256sum $W/nv.raw | cut -d' ' -f1)
taskset -c 28-29 timeout 120 qemu-system-x86_64 -machine q35 -enable-kvm -cpu host -m 2G -smp 2 -bios /usr/share/ovmf/OVMF.fd \
  -drive file=$W/img.raw,format=raw,if=none,id=d0 -device ahci,id=ah -device ide-hd,drive=d0,bus=ah.0 \
  -drive file=$W/nv.raw,format=raw,if=none,id=n0 -device nvme,drive=n0,serial=keep1 \
  -nic none -display none -serial file:$W/serial.log -no-reboot >/dev/null 2>&1
H2=$(sha256sum $W/nv.raw | cut -d' ' -f1)
{
  echo "# public image (pub5, 6fcf24c), KVM -cpu host, SATA boot medium + a 64 MiB NVMe disk of /dev/urandom, 120 s"
  echo "before=$H1"; echo "after =$H2"; [ "$H1" = "$H2" ] && echo NVME-UNTOUCHED || echo NVME-CHANGED
  grep -aE "^\[entropy\]|NVMe|nvme|\[disks\]|interactive loop started|PANIC|panicked|FAULT" $W/serial.log | tr -d '\r' | cut -c1-200 | head -24
  echo "serial lines: $(wc -l < $W/serial.log)"
} > $B/nvme-check.txt
rm -f $W/img.raw $W/nv.raw
echo done > $B/nvme-check.done

# 2. Jitter only: 20 boots under KVM with a CPU model without RDRAND and RDSEED.
./eboots.sh $B/st5.img $B jitteronly-kvm 20 28-29 kvm "qemu64,+smep,+smap,-rdrand,-rdseed" "[3d3]" 900 2
echo done > $B/jitteronly-kvm.done

# 3. Screenshots of the public image at 1920x1080 and 1280x720 (probe.py of the desktop pass).
python3 $U/probe.py $S/gui-smoke.py $B/pub5.img $B/sh-1080 $U/scenarios/after.txt --kvm --pin 28-29 > $B/sh-1080.out 2>&1
python3 $U/probe.py $S/gui-smoke.py $B/pub5.img $B/sh-720 $U/scenarios/after.txt --kvm --pin 28-29 --bootopt display.mode=1280x720 > $B/sh-720.out 2>&1
echo done > $B/shots.done
