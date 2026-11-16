#!/bin/bash
# wave3.sh: the final code eee05a6 (f2): starts against e4b8ba4, KVM boots, gui-smoke, keylat, interaction, boot time. CPUs 4-11 only.
B=/srv/euroos/11fa-runs; cd $B
until [ -f runs/wave2.done ]; do sleep 15; done
w() { sleep 30; until [ $(grep -al "smoke\] \(PASS\|FAIL\|CRASH\)" $(for n in "$@"; do echo runs/$n.out; done) 2>/dev/null | wc -l) -ge $# ]; do sleep 15; done; }
SCRIPTS=s2/scripts ./cms.sh f2-starts20 img/f2-br.img 4-5 --starts 20 --smp 2
SCRIPTS=s2/scripts ./cms.sh b0-starts20 img/b0-br.img 6-7 --starts 20 --smp 2
SCRIPTS=s2/scripts ./cms.sh f2-interact img/f2-br.img 8-9 --only interact --smp 2
for m in 1G 2G 4G 8G; do ./kboot-hda.sh img/f2-st.img /dev/shm/11fa/kvm f2-$m $m 2 10-11 > kvm/f2-$m.res; done
w f2-interact
rm -rf /dev/shm/11fa/gs-pub2; mkdir -p /dev/shm/11fa/gs-pub2
python3 s2/scripts/gui-smoke.py --image img/f2-pub.img --kvm --smp 4 --mem 4G --pin 8-11 --workdir /dev/shm/11fa/gs-pub2 > runs/gs-pub2.out 2>&1
until [ $(grep -a -c "start [0-9]*/20" runs/f2-starts20.out runs/b0-starts20.out | awk -F: "{s+=\$2} END {print s}") -ge 40 ]; do sleep 15; done
sleep 20
SCRIPTS=s2/scripts ./cms.sh kf2-f-4 img/f2-br.img 4-7 --only perf --smp 4; SCRIPTS=s2/scripts ./cms.sh kf2-b0-4 img/b0-br.img 8-11 --only perf --smp 4
w kf2-f-4 kf2-b0-4
SCRIPTS=s2/scripts ./cms.sh kf2-f-2 img/f2-br.img 8-9 --only perf --smp 2; SCRIPTS=s2/scripts ./cms.sh kf2-b0-2 img/b0-br.img 4-5 --only perf --smp 2
w kf2-f-2 kf2-b0-2
mkdir -p /dev/shm/11fa/bt2
for i in 1 2 3 4 5 6 7 8 9 10; do
  python3 s2/scripts/boot-time.py f2-$i img/f2-pub.img /dev/shm/11fa/bt2 --pin 4-5 --smp 2 --mem 2G >> runs/boot-time2.out 2>&1
  python3 s2/scripts/boot-time.py b0-$i /srv/euroos/val-runs/img/v2-pub.img /dev/shm/11fa/bt2 --pin 4-5 --smp 2 --mem 2G >> runs/boot-time2.out 2>&1
done
echo done > runs/wave3.done
