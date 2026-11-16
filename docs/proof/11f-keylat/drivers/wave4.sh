#!/bin/bash
# wave4.sh: final regression of 544d5a2 (f3). CPUs 4-11 only, siblings 24-31 idle.
B=/srv/euroos/11fa-runs; cd $B
w() { sleep 20; until [ $(grep -al "chrome-smoke\] \(PASS\|FAIL\|CRASH\)" $(for n in "$@"; do echo runs/$n.out; done) 2>/dev/null | wc -l) -ge $# ]; do sleep 15; done; }
ws() { until [ $(grep -al "chrome-smoke\] starts:" $(for n in "$@"; do echo runs/$n.out; done) 2>/dev/null | wc -l) -ge $# ]; do sleep 15; done; }
until ! ps -eo args | grep -q "[k]boot-hda.sh img/f[23]-st"; do sleep 5; done
for m in 2G 4G 8G; do ./kboot-hda.sh img/f3-st.img /dev/shm/11fa/kvm f3-$m $m 2 10-11 > kvm/f3-$m.res; done
w f3-interact
SCRIPTS=s2/scripts ./cms.sh f3-sites3 img/f3-br.img 8-9 --only sites --rounds 3 --smp 2
SCRIPTS=s2/scripts ./cms.sh kf3-0-f-2 img/f3-br.img 10-11 --only perf --smp 2
w kf3-0-f-2
ws f3-starts20 b0-starts20
rm -rf /dev/shm/11fa/gs-pub3; mkdir -p /dev/shm/11fa/gs-pub3
python3 s2/scripts/gui-smoke.py --image img/f3-pub.img --kvm --smp 4 --mem 4G --pin 4-7 --workdir /dev/shm/11fa/gs-pub3 > runs/gs-pub3.out 2>&1
w f3-sites3
for r in 1 2 3; do
  if [ $((r % 2)) = 1 ]; then F=4-7; O=8-11; else F=8-11; O=4-7; fi
  SCRIPTS=s2/scripts ./cms.sh kf3-$r-f-4 img/f3-br.img $F --only perf --smp 4; SCRIPTS=s2/scripts ./cms.sh kf3-$r-b0-4 img/b0-br.img $O --only perf --smp 4
  w kf3-$r-f-4 kf3-$r-b0-4
done
for r in 1 2; do
  SCRIPTS=s2/scripts ./cms.sh kf3-$r-f-2a img/f3-br.img 4-5 --only perf --smp 2; SCRIPTS=s2/scripts ./cms.sh kf3-$r-b0-2a img/b0-br.img 8-9 --only perf --smp 2
  SCRIPTS=s2/scripts ./cms.sh kf3-$r-b0-2b img/b0-br.img 6-7 --only perf --smp 2; SCRIPTS=s2/scripts ./cms.sh kf3-$r-f-2b img/f3-br.img 10-11 --only perf --smp 2
  w kf3-$r-f-2a kf3-$r-b0-2a kf3-$r-b0-2b kf3-$r-f-2b
done
mkdir -p /dev/shm/11fa/bt3
for i in 1 2 3 4 5 6 7 8 9 10; do
  python3 s2/scripts/boot-time.py f3-$i img/f3-pub.img /dev/shm/11fa/bt3 --pin 4-5 --smp 2 --mem 2G >> runs/boot-time3.out 2>&1
  python3 s2/scripts/boot-time.py b0-$i /srv/euroos/val-runs/img/v2-pub.img /dev/shm/11fa/bt3 --pin 4-5 --smp 2 --mem 2G >> runs/boot-time3.out 2>&1
done
python3 scmd.py img/f3-pub.img 4-7 4 /dev/shm/11fa/fair-f3 "hogtest 5 10 8;serialcost 8" "] [serial-cost]" > runs/fair-f3.out 2>&1
python3 scmd.py /srv/euroos/val-runs/img/v2-pub.img 8-11 4 /dev/shm/11fa/fair-b0 "hogtest 5 10 8" "] [fair]" > runs/fair-b0.out 2>&1
echo done > runs/wave4.done
