#!/usr/bin/env bash
# kdebug proof runs on the R630 (KVM). Own CPUs, SLIRP restrict=on, no VFIO, no NIC passthrough.
# usage: r630-start.sh soak | boots | boottime
B=/srv/euroos/kdebug-runs
S=$B/scripts
IMG=$B/selftest.img
PUB=$B/image.img
cd $B
case "$1" in
soak)
  # 2 h 05, churn proc 8 every round, 4 httpd clients (selftest image, net.driver=user)
  mkdir -p $B/soak/w
  setsid nohup python3 $S/loadtest.py --image $IMG --workdir $B/soak/w --kvm --smp 4 --pin 8-11 --nic e1000e \
    --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18580 --hammer 4 --procs 32 \
    --scenario soak --soak-churn-proc 8 --duration 7500 --boot-timeout 900 > $B/soak/out.txt 2>&1 < /dev/null &
  ;;
boots)
  # 100 short boots: slot s -> CPUs 12+4s..15+4s
  loop() { # slot count tag extra...
    local s=$1 n=$2 tag=$3; shift 3
    local lo=$((12 + s * 4)); [ $s = 3 ] && lo=0; [ $s = 4 ] && lo=4; local hi=$((lo + 3)); [ $s = 5 ] && lo=38 && hi=39
    mkdir -p $B/$tag
    ( for i in $(seq 1 $n); do
        [ -f $B/STOP ] && break
        W=$B/$tag/s$s-r$i; mkdir -p $W
        timeout 1500 python3 $S/loadtest.py --image $IMG --workdir $W --kvm --smp 4 --mem 4G --pin $lo-$hi \
          --nic e1000e --restrict --nmi-on-hang --boot-timeout 900 --scenario smoke "$@" > $W/out.txt 2>&1
        rc=$?
        L=$(ls $W/loadtest-serial-*.log 2>/dev/null | head -1)
        v=ok
        grep -aq "interactive loop started" "$L" 2>/dev/null || v=nodesktop
        grep -aq "KERNEL PANIC\|GENERAL PROTECTION\|\[idt\] PAGE FAULT\|NMI PROBE\|DOUBLE FAULT" "$L" 2>/dev/null && v=CRASH
        grep -q "completed clean" $W/out.txt || { [ $v = ok ] && v=notclean; }
        fd=$(grep -a "^\[fdspace\]" "$L" | grep -c "OK ✓"); b53=$(grep -a "^\[bug053\]" "$L" | grep -c "OK ✓")
        fails=$(grep -a "^\[[a-z0-9_./-]*\]" "$L" | grep -c "FAILED")
        echo "$(date +%F_%T) slot=$s run=$i rc=$rc $v fdspace-ok=$fd bug053-ok=$b53 failed-lines=$fails" >> $B/$tag/summary.txt
        if [ $v = ok ]; then
          grep -a "^\[fdspace\]\|^\[bug053\]\|^\[ag-lock\]\|^\[lk\] boot-total\|^\[boot-time\] phase lockscreen" "$L" > $W/keylines.txt
          grep -a "^\[[a-z0-9_./-]*\]" "$L" | grep "FAILED" | cut -c1-160 > $W/failed-lines.txt
          rm -f "$L"
        fi
        rm -f $W/*.img $W/*.raw
      done ) > /dev/null 2>&1 < /dev/null &
  }
  loop 0 20 boots-user
  loop 1 20 boots-user
  loop 2 15 boots-user-iommu --iommu
  loop 3 15 boots-user-iommu --iommu
  loop 4 15 boots-smp1 --smp 1
  loop 5 15 boots-smp1 --smp 1
  ;;
boottime)
  mkdir -p $B/bt
  ( for i in 1 2 3 4 5; do python3 $S/boot-time.py default-$i $PUB $B/bt --pin 36-37 --hold 20; done > $B/bt/kvm-5x.txt 2>&1
    for i in 1 2 3 4 5; do python3 $S/boot-time.py modern-$i $PUB $B/bt --mode modern --pin 36-37 --hold 20; done >> $B/bt/kvm-5x.txt 2>&1
    echo done > $B/bt/kvm-5x.done ) < /dev/null > /dev/null 2>&1 &
  ;;
esac
disown -a
sleep 2
echo started $1
