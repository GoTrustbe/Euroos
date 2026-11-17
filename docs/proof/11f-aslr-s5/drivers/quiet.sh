#!/bin/bash
B=/srv/euroos/aslr5-runs; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs
mkdir -p kvmq
( for i in 1 2 3; do ./kboot-hda.sh $I/s5-st-leg.img $B/kvmq s5leg-1G-$i 1G 2 8-9 "interactive loop started" 900 > kvmq/s5leg-1G-$i.res 2>&1; done ) &
( for i in 1 2 3; do ./kboot-hda.sh $I/m-st-leg.img $B/kvmq mleg-1G-$i 1G 2 10-11 "interactive loop started" 900 > kvmq/mleg-1G-$i.res 2>&1; done ) &
N=soak-on-q; rm -rf runs/$N /dev/shm/a5/$N; mkdir -p /dev/shm/a5/$N; ln -s /dev/shm/a5/$N runs/$N
python3 $S/chrome-mem-soak.py --image $I/s5-br.img --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin 4-5 --smp 2 --mem 8192M --workdir /dev/shm/a5/$N --no-video --free-video-seconds 0 --hours 1 --local-video-dir $B/vid --local-seconds 60 --local-port 18794 > runs/$N.out 2>&1 < /dev/null
wait; touch quiet.done
