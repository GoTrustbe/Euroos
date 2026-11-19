#!/bin/bash
# phase2.sh: after the stress boots: markers boots fix vs s6 base (1/2/4/8 GiB, 2 vCPU), gui-smoke (public image),
# Chromium 20 starts and sites 3 rounds (browser image). CPUs 14-17, 28-31, 34-37 only.
B=/srv/euroos/b390-runs; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs
while [ $(wc -l < runs/stress/summary.txt) -lt 90 ] || ps -eo args | grep -q "^/bin/bash ./stress.sh"; do sleep 30; done
mkdir -p /dev/shm/b390 runs/kvm
cs() { python3 $S/chrome-smoke.py --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --smp 2 "$@"; }
klane() { PIN=$1 T=$2; shift 2; for M in "$@"; do ./kboot.sh $I/$T.img runs/kvm/$T k-$M $M 2 $PIN "interactive loop started" 900 120 > runs/kvm/$T-$M.res 2>&1; done; }
klane 14,15 fix-st 1G 2G 4G 8G &
klane 16,17 s6-st 1G 2G 4G 8G &
( mkdir -p runs/gs-fix; python3 $S/gui-smoke.py --image $I/fix-pub.img --workdir $B/runs/gs-fix --kvm --smp 4 --mem 4G --pin 34,35,36,37 > runs/gs-fix.out 2>&1 < /dev/null ) &
( N=starts20; mkdir -p /dev/shm/b390/$N; ln -sfn /dev/shm/b390/$N runs/$N; cs --image $I/fix-br.img --pin 28,29 --workdir /dev/shm/b390/$N --starts 20 > runs/$N.out 2>&1 < /dev/null ) &
( N=sites3; mkdir -p /dev/shm/b390/$N; ln -sfn /dev/shm/b390/$N runs/$N; cs --image $I/fix-br.img --pin 30,31 --workdir /dev/shm/b390/$N --only sites --rounds 3 > runs/$N.out 2>&1 < /dev/null ) &
wait; touch phase2.done
