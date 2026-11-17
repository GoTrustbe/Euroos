#!/bin/bash
B=/srv/euroos/aslr5-runs; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs
run() { N=$1 IMG=$2 PIN=$3; rm -rf runs/$N /dev/shm/a5/$N; mkdir -p /dev/shm/a5/$N; ln -s /dev/shm/a5/$N runs/$N
  python3 $S/chrome-smoke.py --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --smp 2 --image $IMG --pin $PIN --workdir /dev/shm/a5/$N --only sites --rounds 3 > runs/$N.out 2>&1 < /dev/null; }
run sites-on-2 $I/s5-br.img 0-1 &
run sites-base $I/m-br.img 2-3 &
wait; touch sitesab.done
