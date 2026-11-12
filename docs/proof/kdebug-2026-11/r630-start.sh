#!/usr/bin/env bash
# kdebug proof runs on the R630 (KVM), final image. Own CPUs, SLIRP restrict=on, no VFIO, no NIC passthrough.
# usage: r630-start2.sh soak | boots
B=/srv/euroos/kdebug-runs
S=$B/scripts
IMG=$B/selftest-final.img
cd $B
case "$1" in
soak)
  # 2 h 05, churn proc 8 every round, 4 httpd clients (selftest image, net.driver=user)
  mkdir -p $B/soak2/w
  setsid nohup python3 $S/loadtest.py --image $IMG --workdir $B/soak2/w --kvm --smp 4 --pin 4-7 --nic e1000e \
    --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18581 --hammer 4 --procs 32 \
    --scenario soak --soak-churn-proc 8 --duration 7500 --boot-timeout 900 > $B/soak2/out.txt 2>&1 < /dev/null &
  ;;
boots)
  # 100 short boots: 40 user, 30 user + IOMMU, 30 smp 1
  loop() { # slot pins count tag extra...
    local s=$1 pins=$2 n=$3 tag=$4; shift 4
    mkdir -p $B/$tag
    ( for i in $(seq 1 $n); do
        [ -f $B/STOP2 ] && break
        W=$B/$tag/s$s-r$i; mkdir -p $W
        timeout 1500 python3 $S/loadtest.py --image $IMG --workdir $W --kvm --smp 4 --mem 4G --pin $pins \
          --nic e1000e --restrict --nmi-on-hang --boot-timeout 900 --scenario smoke "$@" > $W/out.txt 2>&1
        rc=$?
        L=$(ls $W/loadtest-serial-*.log 2>/dev/null | head -1)
        v=ok
        grep -aq "interactive loop started" "$L" 2>/dev/null || v=nodesktop
        grep -aq "KERNEL PANIC\|GENERAL PROTECTION\|\[idt\] PAGE FAULT\|NMI PROBE\|DOUBLE FAULT" "$L" 2>/dev/null && v=CRASH
        grep -q "completed clean" $W/out.txt || { [ $v = ok ] && v=notclean; }
        fd=$(grep -a "^\[fdspace\]" "$L" | grep -c "OK ✓"); b53=$(grep -a "^\[bug053\]" "$L" | grep -c "OK ✓"); b63=$(grep -a "^\[bug063\]" "$L" | grep -c "OK ✓")
        fails=$(grep -a "^\[[a-z0-9_./-]*\]" "$L" | grep -c "FAILED")
        echo "$(date +%F_%T) slot=$s run=$i rc=$rc $v fdspace-ok=$fd bug053-ok=$b53 bug063-ok=$b63 failed-lines=$fails" >> $B/$tag/summary.txt
        if [ $v = ok ]; then
          grep -a "^\[fdspace\]\|^\[bug053\]\|^\[bug063\]\|^\[ag-lock\]\|^\[lk\] boot-total order\|^\[boot-time\] phase lockscreen" "$L" > $W/keylines.txt
          grep -a "^\[[a-z0-9_./-]*\]" "$L" | grep "FAILED" | cut -c1-160 > $W/failed-lines.txt
          rm -f "$L"
        fi
        rm -f $W/*.img $W/*.raw
      done ) > /dev/null 2>&1 < /dev/null &
  }
  loop 0 0-3 20 boots2-user
  loop 1 12-15 20 boots2-user
  loop 2 16-19 15 boots2-user-iommu --iommu
  loop 3 20-23 15 boots2-user-iommu --iommu
  loop 4 24-27 15 boots2-smp1 --smp 1
  loop 5 36-39 15 boots2-smp1 --smp 1
  ;;
esac
disown -a
sleep 2
echo started $1
