#!/bin/bash
B=/srv/euroos/aslr5-runs; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs
run() { N=$1 IMG=$2 PIN=$3 PORT=$4; rm -rf runs/$N /dev/shm/a5/$N; mkdir -p /dev/shm/a5/$N; ln -s /dev/shm/a5/$N runs/$N
  python3 $S/chrome-mem-soak.py --image $IMG --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin $PIN --smp 2 --mem 8192M --workdir /dev/shm/a5/$N --no-video --free-video-seconds 0 --hours 1 --local-video-dir $B/vid --local-seconds 60 --local-port $PORT > runs/$N.out 2>&1 < /dev/null; }
run soak-on-3 $I/s5-br.img 4-5 18795 &
run soak-base $I/m-br.img 6-7 18796 &
wait; touch soakab.done
