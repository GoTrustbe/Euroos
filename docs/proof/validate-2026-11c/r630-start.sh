#!/usr/bin/env bash
# Validation runs on the R630 (KVM), images of the validated commit.
# Own work directory, own CPUs, at most 14 vCPUs at once (soak 4, two boot lanes
# of 2, chain2 at most 4, 2 left for the hand-driven scenarios), SLIRP
# restrict=on or no NIC, no VFIO, no passthrough. Every QEMU is started and
# stopped by the harness that owns it (by PID).
# usage: r630-start.sh soak | boots | chain2
B=/srv/euroos/validate-runs
S=$B/scripts
cd $B || exit 1
case "$1" in
soak)
  # >= 2 h, churn proc 8 every round, 4 httpd clients (self-test image, net.driver=user)
  mkdir -p $B/soak/w
  setsid nohup python3 $S/loadtest.py --image $B/st.img --workdir $B/soak/w --kvm --smp 4 --pin 4-7 --nic e1000e \
    --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18691 --hammer 4 --procs 32 \
    --scenario soak --soak-churn-proc 8 --duration 7500 --boot-timeout 900 > $B/soak/out.txt 2>&1 < /dev/null &
  ;;
boots)
  # 100 short boots of the self-test image, each to the [rename] verdict (it comes
  # right after [linux-rand], which comes after [3d8], [3d7] and [3d3]).
  (
    bash $S/short-boots.sh $B/st.img $B short-a 50 12-13 kvm host "[rename] rename over" 600 2 &
    bash $S/short-boots.sh $B/st.img $B short-b 50 14-15 kvm host "[rename] rename over" 600 2 &
    wait
    echo "done $(date +%T)" > $B/boots.done
  ) > /dev/null 2>&1 < /dev/null &
  ;;
chain2)
  (
    # 1. The public image boots beside a 64 MiB NVMe disk full of random data: the disk is not written.
    W=$B/nvme-check; rm -rf $W; mkdir -p $W
    cp --sparse=always $B/pub.img $W/img.raw
    dd if=/dev/urandom of=$W/nv.raw bs=1M count=64 status=none
    H1=$(sha256sum $W/nv.raw | cut -d' ' -f1)
    taskset -c 20-21 timeout 120 qemu-system-x86_64 -machine q35 -enable-kvm -cpu host -m 2G -smp 2 -bios /usr/share/ovmf/OVMF.fd \
      -drive file=$W/img.raw,format=raw,if=none,id=d0 -device ahci,id=ah -device ide-hd,drive=d0,bus=ah.0 \
      -drive file=$W/nv.raw,format=raw,if=none,id=n0 -device nvme,drive=n0,serial=keep1 \
      -nic none -display none -serial file:$W/serial.log -no-reboot >/dev/null 2>&1
    H2=$(sha256sum $W/nv.raw | cut -d' ' -f1)
    {
      echo "# public image, KVM -cpu host, SATA boot medium + a 64 MiB NVMe disk of /dev/urandom, 120 s"
      echo "before=$H1"; echo "after =$H2"; [ "$H1" = "$H2" ] && echo NVME-UNTOUCHED || echo NVME-CHANGED
      grep -aE "^\[entropy\]|NVMe|nvme|\[disks\]|interactive loop started|PANIC|panicked|FAULT" $W/serial.log | tr -d '\r' | cut -c1-200 | head -24
      echo "serial lines: $(wc -l < $W/serial.log)"
    } > $B/nvme-check.txt
    rm -f $W/img.raw $W/nv.raw
    echo done > $B/nvme-check.done
    # 2. Boot time of the public image, 5 boots.
    mkdir -p $B/bt
    for i in 1 2 3 4 5; do
      python3 $S/boot-time.py final-$i $B/pub.img $B/bt --pin 38-39 --hold 5
    done > $B/bt/kvm.txt 2>&1
    rm -f $B/bt/*.d/boot.img
    echo "done $(date +%T)" > $B/bt.done
    # 3. Monkey soaks of the public image, 35 minutes each after the smoke pass.
    python3 $S/gui-smoke.py --image $B/pub.img --workdir $B/gs-usb --kvm --smp 4 --mem 4G --pin 20-23 --soak 35 --seed 20261114 > $B/gs-usb.out 2>&1
    echo "rc=$? $(date +%T)" > $B/gs-usb.done
    python3 $S/gui-smoke.py --image $B/pub.img --workdir $B/gs-virtio --kvm --smp 2 --mem 2G --pin 20-21 --input virtio --net restrict --soak 35 --seed 20261115 > $B/gs-virtio.out 2>&1
    echo "rc=$? $(date +%T)" > $B/gs-virtio.done
    rm -f $B/gs-*/*.img
    echo "done $(date +%T)" > $B/chain2.done
  ) > /dev/null 2>&1 < /dev/null &
  ;;
*) echo "usage: $0 soak | boots | chain2"; exit 2 ;;
esac
disown -a
sleep 2
echo started $1
