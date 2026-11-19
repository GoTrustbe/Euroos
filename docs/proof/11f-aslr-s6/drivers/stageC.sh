#!/bin/bash
# stageC.sh: soak A/B again with a clip directory of open codecs only (vid-open: VP9 + Opus, no H.264).
B=/srv/euroos/aslr6-runs; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs
until [ -f B3.done ]; do sleep 30; done
shm() { rm -rf runs/$1 /dev/shm/a6/$1; mkdir -p /dev/shm/a6/$1 runs; ln -s /dev/shm/a6/$1 runs/$1; }
soak() { N=$1 IMG=$2 PIN=$3 PORT=$4; shm $N
  python3 $S/chrome-mem-soak.py --image $IMG --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin $PIN --smp 2 --mem 8192M --workdir /dev/shm/a6/$N --no-video --free-video-seconds 0 --hours 1 --local-video-dir $B/vid-open --local-seconds 60 --local-port $PORT > runs/$N.out 2>&1 < /dev/null; }
soak soak2-on $I/s6-br.img 0-1 18803 &
soak soak2-base $I/base-br.img 2-3 18804 &
wait; touch stageC.done
