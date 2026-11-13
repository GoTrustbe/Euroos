#!/usr/bin/env bash
# Second round on the R630 (KVM): the images of the last code commit (stf.img, pubf.img).
# Own work directory, own CPUs, at most 14 vCPUs at once together with what is still
# running of the first round, SLIRP restrict=on or no NIC, no VFIO, no passthrough.
# Every QEMU is started and stopped by the harness that owns it (by PID).
# usage: r630-final.sh soak | boots | chain2
B=/srv/euroos/validate-runs
S=$B/scripts
F=$B/final; mkdir -p $F
cd $B || exit 1
case "$1" in
soak)
  mkdir -p $F/soak/w
  setsid nohup python3 $S/loadtest.py --image $B/stf.img --workdir $F/soak/w --kvm --smp 4 --pin 8-11 --nic e1000e \
    --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18692 --hammer 4 --procs 32 \
    --scenario soak --soak-churn-proc 8 --duration 7500 --boot-timeout 900 > $F/soak/out.txt 2>&1 < /dev/null &
  ;;
boots)
  # 100 short boots, three lanes, after the boots of the first round are done.
  (
    until [ -f $B/boots.done ]; do sleep 20; done
    bash $S/short-boots.sh $B/stf.img $F short-a 34 12-13 kvm host "[rename] rename over" 600 2 &
    bash $S/short-boots.sh $B/stf.img $F short-b 33 14-15 kvm host "[rename] rename over" 600 2 &
    bash $S/short-boots.sh $B/stf.img $F short-c 33 16-17 kvm host "[rename] rename over" 600 2 &
    wait
    echo "done $(date +%T)" > $F/boots.done
  ) > /dev/null 2>&1 < /dev/null &
  ;;
chain2)
  (
    until [ -f $B/chain2.done ] && [ -f $B/boots.done ] && ! kill -0 1751672 2>/dev/null; do sleep 20; done
    # 1. NVMe untouched
    W=$F/nvme-check; rm -rf $W; mkdir -p $W
    cp --sparse=always $B/pubf.img $W/img.raw
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
    } > $F/nvme-check.txt
    rm -f $W/img.raw $W/nv.raw
    # 2. Boot time: the base (c6f033f plus the probe in /bin) and the final public image, alternating.
    mkdir -p $F/bt
    for i in 1 2 3 4 5; do
      python3 $S/boot-time.py base-$i $B/pubbase.img $F/bt --pin 38-39 --hold 5
      python3 $S/boot-time.py final-$i $B/pubf.img $F/bt --pin 38-39 --hold 5
    done > $F/bt/kvm-ab.txt 2>&1
    for n in base-1 base-2 base-3 base-4 base-5 final-1 final-2 final-3 final-4 final-5; do
      echo "$n: $(grep -a 'desktop after' $F/bt/$n.tlog | sed -E 's/.*(desktop after [0-9]+ ms).*/\1/' | tail -1)"
    done >> $F/bt/kvm-ab.txt
    rm -f $F/bt/*.d/boot.img
    echo "done $(date +%T)" > $F/bt.done
    # 3. Monkey soaks, 35 minutes each after the smoke pass.
    python3 $S/gui-smoke.py --image $B/pubf.img --workdir $F/gs-usb --kvm --smp 4 --mem 4G --pin 20-23 --soak 35 --seed 20261114 > $F/gs-usb.out 2>&1
    echo "rc=$? $(date +%T)" > $F/gs-usb.done
    python3 $S/gui-smoke.py --image $B/pubf.img --workdir $F/gs-virtio --kvm --smp 2 --mem 2G --pin 20-21 --input virtio --net restrict --soak 35 --seed 20261115 > $F/gs-virtio.out 2>&1
    echo "rc=$? $(date +%T)" > $F/gs-virtio.done
    rm -f $F/gs-*/*.img
    echo "done $(date +%T)" > $F/chain2.done
  ) > /dev/null 2>&1 < /dev/null &
  ;;
*) echo "usage: $0 soak | boots | chain2"; exit 2 ;;
esac
disown -a
sleep 2
echo started $1
