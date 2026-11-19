#!/bin/bash
# sitesbase.sh: one sites round with the S6 base browser image (55c6e25) beside the fix, for the YouTube verdict. CPUs 32,33.
B=/srv/euroos/b390-runs; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs
while [ $(wc -l < runs/stress/summary.txt) -lt 90 ] || ps -eo args | grep -q "^/bin/bash ./stress.sh"; do sleep 30; done
N=sites-s6; mkdir -p /dev/shm/b390/$N; ln -sfn /dev/shm/b390/$N runs/$N
python3 $S/chrome-smoke.py --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --smp 2 --image $I/s6-br.img --pin 32,33 --workdir /dev/shm/b390/$N --only sites --rounds 1 > runs/$N.out 2>&1 < /dev/null
touch sitesbase.done
