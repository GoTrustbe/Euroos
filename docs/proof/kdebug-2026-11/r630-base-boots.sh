#!/usr/bin/env bash
# The same short boots with the BASE self-test image (7858b72), to see how often the occasional
# FAILED lines of the boot loops ([3d8], [3d3], [3d7]) occur without this branch.
B=/srv/euroos/kdebug-runs
S=$B/scripts
IMG=$B/base-self.img
cd $B
loop() { # slot pins count tag extra...
  local s=$1 pins=$2 n=$3 tag=$4; shift 4
  mkdir -p $B/$tag
  ( for i in $(seq 1 $n); do
      [ -f $B/STOP0 ] && break
      W=$B/$tag/s$s-r$i; mkdir -p $W
      timeout 1500 python3 $S/loadtest.py --image $IMG --workdir $W --kvm --smp 4 --mem 4G --pin $pins \
        --nic e1000e --restrict --nmi-on-hang --boot-timeout 900 --scenario smoke "$@" > $W/out.txt 2>&1
      rc=$?
      L=$(ls $W/loadtest-serial-*.log 2>/dev/null | head -1)
      v=ok
      grep -aq "interactive loop started" "$L" 2>/dev/null || v=nodesktop
      grep -aq "KERNEL PANIC\|GENERAL PROTECTION\|\[idt\] PAGE FAULT\|NMI PROBE\|DOUBLE FAULT" "$L" 2>/dev/null && v=CRASH
      grep -q "completed clean" $W/out.txt || { [ $v = ok ] && v=notclean; }
      fails=$(grep -a "^\[[a-z0-9_./-]*\]" "$L" | grep -c "FAILED")
      echo "$(date +%F_%T) slot=$s run=$i rc=$rc $v failed-lines=$fails" >> $B/$tag/summary.txt
      grep -a "^\[[a-z0-9_./-]*\]" "$L" | grep "FAILED" | tr -d "\r" | cut -c1-700 > $W/failed-lines.txt
      grep -a "^\[3d8\]\|^\[3d3\]\|^\[3d7\]" "$L" | tr -d "\r" | cut -c1-700 > $W/3d-lines.txt
      [ $v = ok ] && rm -f "$L"
      rm -f $W/*.img $W/*.raw
    done ) > /dev/null 2>&1 < /dev/null &
}
loop 0 0-3 10 boots0-user
loop 1 12-15 10 boots0-user
loop 2 16-19 10 boots0-user-iommu --iommu
loop 3 20-23 10 boots0-user-iommu --iommu
loop 4 24-27 10 boots0-smp1 --smp 1
loop 5 36-39 10 boots0-smp1 --smp 1
disown -a
sleep 2
echo started
