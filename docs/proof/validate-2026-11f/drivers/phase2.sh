#!/bin/bash
# phase2.sh: long runs + tmpfs-only lanes. Soak CPUs 30-33 (siblings 10-13 idle), browser session CPUs 0-1 (siblings 20-21 idle),
# nothing disk-heavy beside the soak (no installs, no TCG, no q35, no short boots): those are phase 3.
B=/srv/euroos/v11f-runs; cd $B
waitdone() { until grep -aq "smoke\] [A-Z]* - [0-9]*/[0-9]* steps ok" runs/$1.out 2>/dev/null; do sleep 15; done; }
mkdir -p soak && (cd soak && setsid nohup python3 -u $B/scripts/loadtest.py --image $B/img/m-st.img --workdir $B/soak/w --kvm --smp 4 --pin 30-33 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18501 --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 7500 --boot-timeout 900 > out.txt 2>&1 < /dev/null &)
N=soak2h; rm -rf runs/$N /dev/shm/v11f/$N; mkdir -p /dev/shm/v11f/$N; ln -s /dev/shm/v11f/$N runs/$N
(setsid nohup python3 scripts/chrome-mem-soak.py --image img/m-br.img --pack /srv/euroos/chrome-runs/chrome-pack2.img --nss /srv/euroos/chrome-runs/nss-pack-vid.img --kvm --pin 0-1 --smp 2 --mem 8192M --workdir /dev/shm/v11f/$N --no-video --free-video-seconds 0 --hours 2 --local-video-dir /srv/euroos/v11f-runs/vid --local-seconds 60 --local-port 18782 > runs/$N.out 2>&1 < /dev/null &)
gs() { # name img smp pins extra...
  n=$1; im=$2; smp=$3; pins=$4; shift 4; rm -rf runs/gs-$n /dev/shm/v11f/gs-$n; mkdir -p /dev/shm/v11f/gs-$n; ln -s /dev/shm/v11f/gs-$n runs/gs-$n
  python3 scripts/gui-smoke.py --image $im --workdir /dev/shm/v11f/gs-$n --kvm --smp $smp --mem 4G --pin $pins "$@" > runs/gs-$n.out 2>&1 < /dev/null; }
laneA() {
  gs pub img/m-pub.img 4 14-17
  gs soak img/m-pub.img 4 14-17 --soak 35 --seed 20261114
  gs input img/m-pub.img 4 14-17 --input virtio
  gs e1000e img/m-pub.img 2 14-15 --net e1000e
  gs smp img/m-pubsmp.img 4 14-17
  gs b0 img/b0-pub.img 4 14-17
  touch laneA.done; }
laneB() {
  for t in m b0; do mkdir -p /dev/shm/v11f/kvm/$t; for g in 1 2 4 8; do ./kboot-hda.sh $B/img/$t-st.img /dev/shm/v11f/kvm/$t k-${g}g ${g}G 2 14-15 "interactive loop started" 900 > /dev/shm/v11f/kvm/$t/k-${g}g.res 2>&1; done; done
  mkdir -p /dev/shm/v11f/kvm/s4; for t in m b0; do ./kboot-hda.sh $B/img/$t-st.img /dev/shm/v11f/kvm/s4 $t-4g 4G 4 14-17 "interactive loop started" 900 > /dev/shm/v11f/kvm/s4/$t-4g.res 2>&1; done
  touch laneB.done; }
laneC1() {
  N=starts20-m; rm -rf runs/$N /dev/shm/v11f/$N; mkdir -p /dev/shm/v11f/$N; ln -s /dev/shm/v11f/$N runs/$N
  python3 scripts/chrome-smoke.py --image img/m-br.img --pack /srv/euroos/chrome-runs/chrome-pack2.img --nss /srv/euroos/chrome-runs/nss-pack-vid.img --kvm --pin 2-3 --workdir /dev/shm/v11f/$N --starts 20 --smp 2 > runs/$N.out 2>&1 < /dev/null
  ./cms.sh sites3 img/m-br.img 2-3 --only sites --rounds 3 --smp 2; waitdone sites3
  ./cms.sh interact img/m-br.img 2-3 --only interact --smp 2; waitdone interact
  ./cms.sh localvideo img/m-br.img 2-3 --only localvideo --smp 2 --media-dir /srv/euroos/harness-runs/vid --local-port 18781; waitdone localvideo
  N=wm480; rm -rf runs/$N /dev/shm/v11f/$N; mkdir -p /dev/shm/v11f/$N; ln -s /dev/shm/v11f/$N runs/$N
  python3 scripts/wmvideo.py --image img/m-br.img --pack /srv/euroos/chrome-runs/chrome-pack2.img --nss /srv/euroos/chrome-runs/nss-pack-vid.img --workdir /dev/shm/v11f/$N --pin 2-3 --kvm --smp 2 --mem 6144M --seconds 90 --times 2 --url https://upload.wikimedia.org/wikipedia/commons/transcoded/c/c0/Big_Buck_Bunny_4K.webm/Big_Buck_Bunny_4K.webm.480p.vp9.webm > runs/$N.out 2>&1 < /dev/null
  touch laneC1.done; }
laneT() {
  mkdir -p /dev/shm/v11f/bt; : > bt-ab.txt
  for i in $(seq 1 12); do for t in m b0; do
    python3 scripts/boot-time.py $t-$i img/$t-pub.img /dev/shm/v11f/bt --pin 8-9 --smp 2 --mem 2G 2>&1 | tail -1 >> bt-ab.txt; rm -rf /dev/shm/v11f/bt/$t-$i.d
  done; done
  touch bt.done
  mkdir -p fair; for t in m b0; do for n in $(seq 1 10); do N=$t-$n; rm -rf /dev/shm/v11f/fair/$N; mkdir -p /dev/shm/v11f/fair/$N
    (cd /dev/shm/v11f/fair/$N; python3 -u $B/scripts/loadtest.py --image $B/img/$t-st.img --workdir $PWD --kvm --smp 4 --pin 4-7 --mem 4G --nic e1000e --restrict --scenario cmds --cmds "hogtest 5 10 8" --boot-timeout 900 > out.txt 2>&1)
    grep -a "^\[fair\]" /dev/shm/v11f/fair/$N/loadtest-serial-*.log > fair/$N.txt; rm -rf /dev/shm/v11f/fair/$N; done; done
  touch laneT.done; }
(laneB; laneA) & laneC1 & laneT & wait
echo phase2-lanes-done > phase2.done
