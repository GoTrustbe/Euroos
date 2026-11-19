#!/bin/bash
# after final.sh: scheduler A/B, B (f487cc7) against base (19f7586): [fair] (hogtest 5 10 8, public, 4 vCPU) and keylat (chrome-smoke --only perf, smp 2)
B=/srv/euroos/b400-runs; cd $B; S=$B/scripts; mkdir -p sched
until [ -e final.done ] && [ -e img/base-pub.img ] && [ -e img/base-br.img ]; do sleep 30; done
for r in 1 2; do
  for v in b base; do python3 scmd.py img/$v-pub.img 18,19,38,39 4 /dev/shm/b400/fair-$v-$r "hogtest 5 10 8" "rounds of hogtest" > sched/fair-$v-$r.out 2>&1; cp /dev/shm/b400/fair-$v-$r/serial.log sched/fair-$v-$r.log; rm -rf /dev/shm/b400/fair-$v-$r; done
done
kl() { N=$1 IMG=$2 PIN=$3; mkdir -p /dev/shm/b400/$N; python3 $S/chrome-smoke.py --image $IMG --pack /srv/euroos/chrome-runs/chrome-pack2.img --nss /srv/euroos/chrome-runs/nss-pack-vid.img --kvm --pin $PIN --smp 2 --workdir /dev/shm/b400/$N --only perf > sched/$N.out 2>&1 < /dev/null; cp /dev/shm/b400/$N/serial.log sched/$N.log 2>/dev/null; rm -rf /dev/shm/b400/$N; }
kl k1-b img/b-br.img 18,38 & kl k1-base img/base-br.img 19,39 & wait
kl k2-b img/b-br.img 19,39 & kl k2-base img/base-br.img 18,38 & wait
touch final2.done
