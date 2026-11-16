#!/bin/bash
# stage 3 (revised): measurements beside the clean soak rerun (soak2: CPUs 30-33, siblings 10-13 idle): nothing runs on CPUs 10-13 and 30-33, nothing disk-heavy
B=/srv/euroos/reval-runs; cd $B; S=$B/scripts
mkdir -p bt; : > bt/kvm-ab.txt
for i in $(seq 1 12); do for t in v2 b0 v1; do
  if [ $t = v1 ]; then I=/srv/euroos/val-runs/img/v1-pub.img; else I=$B/img/$t-pub.img; fi
  python3 $S/boot-time.py $t-$i $I bt --pin 18-19 --smp 2 2>&1 | tail -1 >> bt/kvm-ab.txt; rm -rf bt/$t-$i.d
done; done
touch bt.done
for r in 1 2 3; do
  ./cms.sh kq$r-b0-4 img/b0-br.img 34-37 --only perf --smp 4; ./cms.sh kq$r-v2-4 img/v2-br.img 2-5 --only perf --smp 4
  ./cms.sh kq$r-b0-2 img/b0-br.img 38-39 --only perf --smp 2; ./cms.sh kq$r-v2-2 img/v2-br.img 28-29 --only perf --smp 2
  sleep 30
  until [ $(grep -al "smoke\] \(PASS\|FAIL\|CRASH\)" runs/kq$r-*.out 2>/dev/null | wc -l) -ge 4 ]; do sleep 15; done
done
touch kq.done
# [fair]: 10 boots, 3 at a time (CPUs 2-5, 6-9, 14-17; the browser session has CPUs 0-1, the soak 30-33 with idle siblings 10-13)
for w in 1 2 3 4; do for g in 0 1 2; do
  case $g in 0) P=2-5;; 1) P=6-9;; 2) P=14-17;; esac
  n=$(( (w-1)*3+g+1 )); [ $n -gt 10 ] && continue; N=f$n; rm -rf fair/$N; mkdir -p fair/$N
  (cd fair/$N; python3 -u $S/loadtest.py --image $B/img/v2-st.img --workdir $PWD --kvm --smp 4 --pin $P --mem 4G --nic e1000e --restrict --scenario cmds --cmds "hogtest 5 10 8" --boot-timeout 900 > out.txt 2>&1) &
done; wait; done
touch fair.done
# 100 short boots on 12 lanes (cores 4-9 and 14-19, both siblings of each core busy), 9 boots per lane = 108
mkdir -p short
for p in 4 6 8 14 16 18; do for q in $p $((p+20)); do
  (bash $S/short-boots.sh $B/img/v2-st.img $B/short l$q 9 $q-$((q+1)) kvm host "[fuzz] Linux-ABI" 600 2 > /dev/null 2>&1 < /dev/null &)
done; done
sleep 60; until [ $(ps -eo args | grep -c "[s]hort-boots.sh") -eq 0 ]; do sleep 15; done
touch short.done
echo stage3-done > stage3.done
