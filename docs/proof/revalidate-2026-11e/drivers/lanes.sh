#!/bin/bash
# lanes.sh: stage 1 of the re-validation (feature/revalidate-2026-11e, code 732e5f5 = v2; 3ecde9e = b0; 06e913e = v1)
B=/srv/euroos/reval-runs; cd $B
wt() { until grep -aq "chrome-smoke\] \(PASS\|FAIL\|CRASH\)\|gui-smoke\] \(PASS\|FAIL\)\|chrome-mem-soak\]" runs/$1.out 2>/dev/null; do sleep 20; done; }
waitdone() { until grep -aq "smoke\] \(PASS\|FAIL\|CRASH\)" runs/$1.out 2>/dev/null; do sleep 15; done; }
cp img/v2-pub.img img/v2-pubsmp.img; printf "smpuser=on\n" > /tmp/rv-smp.conf; mcopy -o -i img/v2-pubsmp.img /tmp/rv-smp.conf ::/EFI/BOOT/euroos-boot.conf
cp img/v2-br.img img/v2-brsmp.img; printf "ui.tour=off\nupdate.check=off\nsmpuser=on\n" > /tmp/rv-smpb.conf; mcopy -o -i img/v2-brsmp.img /tmp/rv-smpb.conf ::/EFI/BOOT/euroos-boot.conf
# long runs: soak (CPUs 30-33, siblings 10-13 idle) and the 2 h browser session (CPUs 0-1, siblings 20-21 idle)
(setsid nohup bash -c "mkdir -p soak && cd soak && python3 -u $B/scripts/loadtest.py --image $B/img/v2-st.img --workdir $B/soak/w --kvm --smp 4 --pin 30-33 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18501 --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 7500 --boot-timeout 900 > out.txt 2>&1" > /dev/null 2>&1 < /dev/null &)
N=soak2h; rm -rf runs/$N /dev/shm/rv/$N; mkdir -p /dev/shm/rv/$N; ln -s /dev/shm/rv/$N runs/$N
(setsid nohup python3 scripts/chrome-mem-soak.py --image img/v2-br.img --pack /srv/euroos/chrome-runs/chrome-pack2.img --nss /srv/euroos/chrome-runs/nss-pack-vid.img --kvm --pin 0-1 --smp 2 --mem 8192M --workdir /dev/shm/rv/$N --no-video --free-video-seconds 0 --hours 2 --local-video-dir /srv/euroos/harness-runs/vid --local-seconds 60 > runs/$N.out 2>&1 < /dev/null &)
# lane A (CPUs 14-17): gui-smoke public image variants
laneA() {
  mkdir -p runs/gs-pub; python3 scripts/gui-smoke.py --image img/v2-pub.img --workdir $B/runs/gs-pub --kvm --smp 4 --mem 4G --pin 14-17 > runs/gs-pub.out 2>&1 < /dev/null
  mkdir -p runs/gs-soak; python3 scripts/gui-smoke.py --image img/v2-pub.img --workdir $B/runs/gs-soak --kvm --smp 4 --mem 4G --pin 14-17 --soak 35 --seed 20261114 > runs/gs-soak.out 2>&1 < /dev/null
  mkdir -p runs/gs-input; python3 scripts/gui-smoke.py --image img/v2-pub.img --workdir $B/runs/gs-input --kvm --smp 4 --mem 4G --pin 14-17 --input virtio > runs/gs-input.out 2>&1 < /dev/null
  mkdir -p runs/gs-e1000e; python3 scripts/gui-smoke.py --image img/v2-pub.img --workdir $B/runs/gs-e1000e --kvm --smp 2 --mem 4G --pin 14-15 --net e1000e > runs/gs-e1000e.out 2>&1 < /dev/null
  mkdir -p runs/gs-smp; python3 scripts/gui-smoke.py --image img/v2-pubsmp.img --workdir $B/runs/gs-smp --kvm --smp 4 --mem 4G --pin 14-17 > runs/gs-smp.out 2>&1 < /dev/null
  until [ -f img/b0-pub.img ] && [ -f img/b0.done ]; do sleep 30; done
  mkdir -p runs/gs-b0; python3 scripts/gui-smoke.py --image img/b0-pub.img --workdir $B/runs/gs-b0 --kvm --smp 4 --mem 4G --pin 14-17 > runs/gs-b0.out 2>&1 < /dev/null
  touch laneA.done; }
# lane B (CPUs 18-19): KVM self-test boots, q35 matrix
laneB() {
  mkdir -p kvm/v2 kvm/b0
  for m in 1 2 4 8; do ./kboot-hda.sh $B/img/v2-st.img $B/kvm/v2 k-${m}g ${m}G 2 18-19 "interactive loop started" 900 > kvm/v2/k-${m}g.res 2>&1; done
  mkdir -p q35v2 && cp img/v2-st.img q35v2/st.raw && (cd q35v2 && EK_RESTRICT=1 EK_ACCEL=kvm taskset -c 18-19 python3 $B/scripts/run-metal-matrix.py $B/q35v2/st.raw > matrix.out 2>&1)
  until [ -f img/b0.done ]; do sleep 30; done
  for m in 1 2 4 8; do ./kboot-hda.sh $B/img/b0-st.img $B/kvm/b0 k-${m}g ${m}G 2 18-19 "interactive loop started" 900 > kvm/b0/k-${m}g.res 2>&1; done
  touch laneB.done; }
# lane C1 (CPUs 2-3, siblings idle): Chromium, v2 browser image, smp 2
laneC1() {
  N=starts20-v2; rm -rf runs/$N /dev/shm/rv/$N; mkdir -p /dev/shm/rv/$N; ln -s /dev/shm/rv/$N runs/$N
  python3 scripts/chrome-smoke.py --image img/v2-br.img --pack /srv/euroos/chrome-runs/chrome-pack2.img --nss /srv/euroos/chrome-runs/nss-pack-vid.img --kvm --pin 2-3 --workdir /dev/shm/rv/$N --starts 20 --smp 2 > runs/$N.out 2>&1 < /dev/null
  ./cms.sh sites3 img/v2-br.img 2-3 --only sites --rounds 3 --smp 2; waitdone sites3
  ./cms.sh interact img/v2-br.img 2-3 --only interact --smp 2; waitdone interact
  ./cms.sh localvideo img/v2-br.img 2-3 --only localvideo --smp 2 --media-dir /srv/euroos/harness-runs/vid; waitdone localvideo
  N=wm480; rm -rf runs/$N /dev/shm/rv/$N; mkdir -p /dev/shm/rv/$N; ln -s /dev/shm/rv/$N runs/$N
  python3 scripts/wmvideo.py --image img/v2-br.img --pack /srv/euroos/chrome-runs/chrome-pack2.img --nss /srv/euroos/chrome-runs/nss-pack-vid.img --workdir /dev/shm/rv/$N --pin 2-3 --kvm --smp 2 --mem 6144M --seconds 90 --times 2 > runs/$N.out 2>&1 < /dev/null
  touch laneC1.done; }
# lane C2 (CPUs 4-5): 3ecde9e browser image, 20 starts next to it
laneC2() {
  until [ -f img/b0.done ]; do sleep 30; done
  N=starts20-b0; rm -rf runs/$N /dev/shm/rv/$N; mkdir -p /dev/shm/rv/$N; ln -s /dev/shm/rv/$N runs/$N
  python3 scripts/chrome-smoke.py --image img/b0-br.img --pack /srv/euroos/chrome-runs/chrome-pack2.img --nss /srv/euroos/chrome-runs/nss-pack-vid.img --kvm --pin 4-5 --workdir /dev/shm/rv/$N --starts 20 --smp 2 > runs/$N.out 2>&1 < /dev/null
  touch laneC2.done; }
# lane N (noisy zone CPUs 6-9 + 26-29: nothing measured): TCG matrix, installs, wizard, ro-install, rename-persist
laneN() {
  mkdir -p tcg/v2; ./tcgmatrix.sh v2 $B/img/v2-st.img 6-9 26-27
  mkdir -p inst; for i in $(seq 1 10); do (python3 inst.py img/v2-pub.img inst/v2-$i 6-9,26-29 > inst/v2-$i.out 2>&1 < /dev/null &); done
  until [ $(cat inst/v2-*.out 2>/dev/null | grep -c "^\[inst\]") -ge 10 ]; do sleep 10; done
  taskset -c 6-9,26-29 python3 scripts/wizard-install-test.py img/v2-pub.img $B/wizard > wizard.out 2>&1 < /dev/null
  for m in plain inject ro0; do mkdir -p roi-$m; MODE=$m python3 scripts/ro-install-test.py img/v2-pub.img roi-$m 6-9 > roi-$m.out 2>&1 < /dev/null; done
  mkdir -p rp; python3 scripts/rename-persist.py --image img/v2-pub.img --workdir $B/rp --wizard scripts/wizard-install-test.py --kvm --pin 6-9 > rp.out 2>&1 < /dev/null
  touch laneN.done; }
laneA & laneB & laneC1 & laneC2 & laneN &
wait
echo stage1-done > stage1.done
