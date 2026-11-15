#!/bin/bash
B=/srv/euroos/sched-runs; cd $B
until grep -q "matrix f done" tcg/f/queue.txt; do sleep 20; done
wt() { until grep -aq "chrome-smoke\] \(PASS\|FAIL\)\|gui-smoke\] \(PASS\|FAIL\)" runs/$1.out 2>/dev/null; do sleep 20; done; }
./cm.sh sites3 img/fin-br.img 4-7 --only sites --rounds 3
mkdir -p runs/gs-pub; (python3 scripts/gui-smoke.py --image img/fin-pub.img --workdir $B/runs/gs-pub --kvm --smp 4 --mem 4G --pin 8-9 > runs/gs-pub.out 2>&1 < /dev/null) 
wt sites3
./cm.sh interact img/fin-br.img 4-7 --only interact
./cm.sh localvideo img/fin-br.img 8-9 --only localvideo --media-dir /srv/euroos/harness-runs/vid
wt interact; wt localvideo
until grep -q "ALL LEGS\|legs failed\|FAIL" q35m/matrix.out && ! pgrep -f "run-metal-matrix.py $B" > /dev/null; do sleep 20; done
mkdir -p kvm
i=0; for m in 1 2 4 8; do case $m in 1) P=4-5;; 2) P=6-7;; 4) P=8-9;; 8) P=10-11;; esac
  ./kboot-hda.sh $B/img/fin-st.img $B/kvm k-${m}g ${m}G 2 $P "interactive loop started" 900 > kvm/k-${m}g.res 2>&1 &
done; wait
echo phaseB-done > phaseB.done
