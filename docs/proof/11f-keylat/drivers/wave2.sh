#!/bin/bash
# wave2.sh: final regression of 11f-A after the keylat pairs. CPUs 4-11 only (siblings 24-31 unused).
B=/srv/euroos/11fa-runs; cd $B
until [ -f runs/klat.done ]; do sleep 15; done
w() { sleep 30; until [ $(grep -al "smoke\] \(PASS\|FAIL\|CRASH\)" $(for n in "$@"; do echo runs/$n.out; done) 2>/dev/null | wc -l) -ge $# ]; do sleep 15; done; }
SCRIPTS=s2/scripts ./cms.sh sites3 img/f-br.img 4-5 --only sites --rounds 3 --smp 2
SCRIPTS=s2/scripts ./cms.sh starts20 img/f-br.img 6-7 --starts 20 --smp 2
SCRIPTS=s2/scripts ./cms.sh interact img/f-br.img 8-9 --only interact --smp 2
for m in 1G 2G 4G 8G; do ./kboot-hda.sh img/f-st.img /dev/shm/11fa/kvm f-$m $m 2 10-11 > kvm/f-$m.res; done
w interact
# gui-smoke, public image, 4 vCPU 4 GiB, on 8-11 once interact and the KVM boots are done
rm -rf /dev/shm/11fa/gs-pub; mkdir -p /dev/shm/11fa/gs-pub
python3 s2/scripts/gui-smoke.py --image img/f-pub.img --kvm --smp 4 --mem 4G --pin 8-11 --workdir /dev/shm/11fa/gs-pub > runs/gs-pub.out 2>&1
w sites3 starts20
# boot time: public images, 2 vCPU 2 GiB, pin 4-5, alone, alternating final and e4b8ba4 (= 732e5f5 code, val-runs/img/v2-pub.img)
mkdir -p /dev/shm/11fa/bt
for i in 1 2 3 4 5 6 7 8 9 10; do
  python3 s2/scripts/boot-time.py f-$i img/f-pub.img /dev/shm/11fa/bt --pin 4-5 --smp 2 --mem 2G >> runs/boot-time.out 2>&1
  python3 s2/scripts/boot-time.py b0-$i /srv/euroos/val-runs/img/v2-pub.img /dev/shm/11fa/bt --pin 4-5 --smp 2 --mem 2G >> runs/boot-time.out 2>&1
done
echo done > runs/wave2.done
