#!/bin/bash
# phaseF.sh TAG: the final regression of the D2 commit whose images are img/TAG-{st,pub,br}.img
T=$1; B=/srv/euroos/d2-runs; cd $B
wt() { until grep -aq "chrome-smoke\] \(PASS\|FAIL\|CRASH\)\|gui-smoke\] \(PASS\|FAIL\)" runs/$1.out 2>/dev/null; do sleep 20; done; }
mkdir -p kvm/$T
for m in 1 2 4 8; do case $m in 1) P=0-1;; 2) P=2-3;; 4) P=4-5;; 8) P=6-7;; esac
  ./kboot-hda.sh $B/img/$T-st.img $B/kvm/$T k-${m}g ${m}G 2 $P "interactive loop started" 900 > kvm/$T/k-${m}g.res 2>&1 &
done
(./tcgmatrix.sh $T $B/img/$T-st.img 20-25 26-29 > /dev/null 2>&1 &)
mkdir -p q35$T && cp img/$T-st.img q35$T/st.raw && (cd q35$T && EK_RESTRICT=1 EK_ACCEL=kvm taskset -c 8-11 python3 $B/scripts/run-metal-matrix.py $B/q35$T/st.raw > matrix.out 2>&1 &)
mkdir -p runs/gs-$T && (python3 scripts/gui-smoke.py --image img/$T-pub.img --workdir $B/runs/gs-$T --kvm --smp 4 --mem 4G --pin 12-15 > runs/gs-$T.out 2>&1 < /dev/null &)
mkdir -p soak$T && (cd soak$T && python3 -u $B/scripts/loadtest.py --image $B/img/$T-st.img --workdir $B/soak$T/w --kvm --smp 4 --pin 30-33 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18201 --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 7500 --boot-timeout 900 > out.txt 2>&1 < /dev/null &)
./cm.sh sites-$T img/$T-br.img 16-19 --only sites --rounds 3 --smp 4
# S4 tests on the final image, after the KVM boots (CPUs 0-7)
( until [ $(cat kvm/$T/*.res 2>/dev/null | grep -c end=) -ge 4 ]; do sleep 20; done
  for x in "s4t 0-3 4 tlbsmp 1000;smpstat" "s4n 4-7 4 smpuser noshoot;tlbsmp 100"; do N=$(echo "$x" | cut -d" " -f1)-$T; P=$(echo "$x" | cut -d" " -f2); S=$(echo "$x" | cut -d" " -f3); C=$(echo "$x" | cut -d" " -f4-); mkdir -p runs/$N; IFS=";" read -ra CMDS <<< "$C"; python3 sched.py img/$T-st.img runs/$N --pin $P --smp $S --mem 4G --timeout 3600 "${CMDS[@]}" > runs/$N.out 2>&1 < /dev/null & done; wait
  for x in "s4k 0-3 4 killsmp 500;killsmp 500;smpstat" "s4a 4-7 4 apuser 2;apuser 1 10000 100 cow;manysmp;spread;apidle;smpstat"; do N=$(echo "$x" | cut -d" " -f1)-$T; P=$(echo "$x" | cut -d" " -f2); S=$(echo "$x" | cut -d" " -f3); C=$(echo "$x" | cut -d" " -f4-); mkdir -p runs/$N; IFS=";" read -ra CMDS <<< "$C"; python3 sched.py img/$T-st.img runs/$N --pin $P --smp $S --mem 4G --timeout 3600 "${CMDS[@]}" > runs/$N.out 2>&1 < /dev/null & done; wait ) &
./cm.sh interact-$T img/$T-br.img 34-37 --only interact --smp 4
wt interact-$T
./cm.sh localvideo-$T img/$T-br.img 34-37 --only localvideo --smp 4 --media-dir /srv/euroos/harness-runs/vid
wt sites-$T
PERFAB=off,on,off,on ./cm.sh perf-$T img/$T-br.img 16-19 --only perf --smp 4
wt localvideo-$T; wt perf-$T
wait
echo phaseF-done > phaseF-$T.done
