#!/usr/bin/env bash
# linux-rand proof runs on the R630 (KVM), image of the final commit.
# Own work directory, own CPUs, at most 12 vCPUs at once (soak 4 + two boot
# loops of 4), SLIRP restrict=on or no NIC, no VFIO, no passthrough. Every QEMU
# is started and stopped by the harness that owns it (by PID).
# usage: r630-start.sh soak | boots | chain2
B=/srv/euroos/rand-runs
S=$B/scripts
IMG=$B/after-st.img
cd $B || exit 1
case "$1" in
soak)
  # 1 h, churn proc 8 every round, 4 httpd clients (self-test image, net.driver=user)
  mkdir -p $B/soak/w
  setsid nohup python3 $S/loadtest.py --image $IMG --workdir $B/soak/w --kvm --smp 4 --pin 4-7 --nic e1000e \
    --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18591 --hammer 4 --procs 32 \
    --scenario soak --soak-churn-proc 8 --duration 3600 --boot-timeout 900 > $B/soak/out.txt 2>&1 < /dev/null &
  ;;
boots)
  # 100 short boots: 40 user, 30 user + IOMMU, then 30 smp 1
  loop() { # slot pins count tag extra...
    local s=$1 pins=$2 n=$3 tag=$4; shift 4
    mkdir -p $B/$tag
    for i in $(seq 1 $n); do
      [ -f $B/STOP ] && break
      W=$B/$tag/s$s-r$i; mkdir -p $W
      timeout 1500 python3 $S/loadtest.py --image $IMG --workdir $W --kvm --smp 4 --mem 4G --pin $pins \
        --nic e1000e --restrict --nmi-on-hang --boot-timeout 900 --scenario smoke "$@" > $W/out.txt 2>&1
      rc=$?
      L=$(ls $W/loadtest-serial-*.log 2>/dev/null | head -1)
      v=ok
      grep -aq "interactive loop started" "$L" 2>/dev/null || v=nodesktop
      grep -aq "KERNEL PANIC\|GENERAL PROTECTION\|\[idt\] PAGE FAULT\|NMI PROBE\|DOUBLE FAULT" "$L" 2>/dev/null && v=CRASH
      grep -q "completed clean" $W/out.txt || { [ $v = ok ] && v=notclean; }
      f8=$(grep -a "^\[3d8\]" "$L" | grep -c FAILED); f7=$(grep -a "^\[3d7\]" "$L" | grep -c FAILED); f3=$(grep -a "^\[3d3\]" "$L" | grep -c FAILED)
      o8=$(grep -a "^\[3d8\]" "$L" | grep -c "OK"); o7=$(grep -a "^\[3d7\]" "$L" | grep -c "OK"); o3=$(grep -a "^\[3d3\]" "$L" | grep -c "OK")
      es=$(grep -a "^\[entropy\] CSPRNG seeded" "$L" | wc -l); en=$(grep -a "^\[entropy\] CSPRNG NOT SEEDED" "$L" | wc -l); el=$(grep -a "^\[entropy\]" "$L" | wc -l)
      lr=$(grep -a "^\[linux-rand\] getrandom" "$L" | grep -c "OK ✓"); lf=$(grep -a "^\[linux-rand\] getrandom" "$L" | grep -c "FAILED")
      fails=$(grep -a "^\[[a-z0-9_./-]*\]" "$L" | grep -c "FAILED")
      echo "$(date +%F_%T) slot=$s run=$i rc=$rc $v 3d8 ok=$o8 failed=$f8 | 3d7 ok=$o7 failed=$f7 | 3d3 ok=$o3 failed=$f3 | entropy lines=$el seeded=$es not-seeded=$en | linux-rand ok=$lr failed=$lf | failed-lines=$fails" >> $B/$tag/summary.txt
      grep -a "^\[entropy\]\|^\[3d8\]\|^\[3d7\]\|^\[3d3\]\|^\[linux-rand\] getrandom\|^\[linux-rand\] .*randprobe: at_random=\|^\[linux-rand\] .*randprobe: getrandom=\|^\[linux-rand\] .*randprobe: stack_guard=\|^\[linux-rand\] .*randprobe: urandom=\|^\[lk\] boot-total order\|^\[boot-time\] desktop after" "$L" | tr -d '\r' > $W/keylines.txt
      grep -a "^\[[a-z0-9_./-]*\]" "$L" | grep "FAILED" | cut -c1-200 > $W/failed-lines.txt
      [ $v = ok ] && [ "$fails" = 0 ] && rm -f "$L"
      rm -f $W/*.img $W/*.raw
    done
  }
  (
    loop 0 12-15 40 boots-user &
    loop 1 16-19 30 boots-user-iommu --iommu &
    wait
    loop 2 12-15 15 boots-smp1 --smp 1 &
    loop 3 16-19 15 boots-smp1 --smp 1 &
    wait
    echo "done $(date +%T)" > $B/boots.done
  ) > /dev/null 2>&1 < /dev/null &
  ;;
chain2)
  # After the boot loops: the same probe before and after the fix under KVM,
  # the boot time of the public image (base and final, alternating), and the
  # GUI smoke pass of the public image.
  (
    until [ -f $B/boots.done ]; do sleep 20; done
    python3 $S/rand-boots.py $B/before-st.img --boots 2 --accel kvm --mem 2G --smp 4 --pin 12-15 --outdir $B/rb-before --timeout 900 --expect predictable > $B/rb-before.txt 2>&1
    echo "rc=$?" >> $B/rb-before.txt
    python3 $S/rand-boots.py $B/after-st.img --boots 3 --accel kvm --mem 2G --smp 4 --pin 12-15 --outdir $B/rb-after --timeout 900 > $B/rb-after.txt 2>&1
    echo "rc=$?" >> $B/rb-after.txt
    python3 $S/rand-boots.py $B/after-st.img --boots 2 --accel kvm --cpu "qemu64,+smep,+smap,-rdrand,-rdseed" --mem 2G --smp 2 --pin 12-13 --outdir $B/rb-after-jitter --timeout 1800 > $B/rb-after-jitter.txt 2>&1
    echo "rc=$?" >> $B/rb-after-jitter.txt
    rm -f $B/rb-*/boot-*/qemu.out
    echo "done $(date +%T)" > $B/rb.done
    mkdir -p $B/bt
    for i in 1 2 3 4 5; do
      python3 $S/boot-time.py base-$i $B/base-pub.img $B/bt --pin 38-39 --hold 5
      python3 $S/boot-time.py final-$i $B/after-pub.img $B/bt --pin 38-39 --hold 5
    done > $B/bt/kvm-ab.txt 2>&1
    rm -f $B/bt/*.d/boot.img
    echo "done $(date +%T)" > $B/bt.done
    python3 $S/gui-smoke.py --image $B/after-pub.img --workdir $B/gs-kvm --kvm --smp 4 --mem 4G --pin 12-15 > $B/gs-kvm.out 2>&1
    echo "rc=$? $(date +%T)" > $B/gs-kvm.done
    python3 $S/gui-smoke.py --image $B/after-pub.img --workdir $B/gs-kvm-e1000e --kvm --smp 4 --mem 4G --pin 12-15 --net e1000e > $B/gs-kvm-e1000e.out 2>&1
    echo "rc=$? $(date +%T)" > $B/gs-kvm-e1000e.done
    rm -f $B/gs-kvm*/*.img
    echo "done $(date +%T)" > $B/chain2.done
  ) > /dev/null 2>&1 < /dev/null &
  ;;
*) echo "usage: $0 soak | boots | chain2"; exit 2 ;;
esac
disown -a
sleep 2
echo started $1
