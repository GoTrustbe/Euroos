#!/bin/bash
# soakwave.sh WAVE: three chrome-mem-soaks of 1 h side by side (fix-br.img), each pinned to two cores of its own.
B=/srv/euroos/b390-runs; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs; W=$1
mkdir -p /dev/shm/b390 runs
soak() { N=$1 PIN=$2 PORT=$3; mkdir -p /dev/shm/b390/$N; ln -sfn /dev/shm/b390/$N runs/$N
  python3 $S/chrome-mem-soak.py --image $I/fix-br.img --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin $PIN --smp 2 --mem 8192M --workdir /dev/shm/b390/$N --no-video --free-video-seconds 0 --hours 1 --local-video-dir $B/vid --local-seconds 60 --local-port $PORT > runs/$N.out 2>&1 < /dev/null; }
soak soak$W-a 8,9 189${W}1 &
soak soak$W-b 10,11 189${W}2 &
soak soak$W-c 12,13 189${W}3 &
wait; touch soakwave$W.done
