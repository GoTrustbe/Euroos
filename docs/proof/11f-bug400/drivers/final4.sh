#!/bin/bash
# after final2: the video soak (sites + local VP9/Opus, 0.5 h) of B and base side by side, for the openstreetmap step
B=/srv/euroos/b400-runs; cd $B; S=$B/scripts; CP=/srv/euroos/chrome-runs
until [ -e final2.done ]; do sleep 30; done
vs() { N=$1 IMG=$2 PIN=$3 PORT=$4; mkdir -p ui/$N; python3 $S/chrome-mem-soak.py --image $IMG --pack $CP/chrome-pack2.img --nss $CP/nss-pack-vid.img --kvm --pin $PIN --smp 2 --mem 8192M --workdir $B/ui/$N --no-video --free-video-seconds 0 --hours 0.5 --local-video-dir $B/vid --local-seconds 60 --local-port $PORT > ui/$N.out 2>&1 < /dev/null; }
vs vid2-b img/b-br.img 19,39 18843 & vs vid2-base img/base-br.img 18,38 18844 & wait
touch final4.done
