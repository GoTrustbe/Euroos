#!/bin/bash
# gui-smoke (public image, 4 vCPU 4 GiB) and Chromium with image B (public + update.check=off, ui.tour=off)
B=/srv/euroos/b380-runs; cd $B; S=$B/scripts; P=/srv/euroos/chrome-runs; mkdir -p ui
cs() { python3 $S/chrome-smoke.py --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --smp 2 --image $B/img/b-br.img "$@"; }
mkdir -p ui/gs; python3 $S/gui-smoke.py --image $B/img/b-pub.img --workdir $B/ui/gs --kvm --smp 4 --mem 4G --pin 16,17,36,37 > ui/gs.out 2>&1 < /dev/null &
( mkdir -p ui/starts; cs --pin 18,38 --workdir $B/ui/starts --starts 20 > ui/starts.out 2>&1 < /dev/null ) &
( mkdir -p ui/r3; cs --pin 19,39 --workdir $B/ui/r3 --only sites --rounds 3 > ui/r3.out 2>&1 < /dev/null ) &
wait
for i in 1 2 3; do mkdir -p ui/s$i; ( cs --pin $((15+i)),$((35+i)) --workdir $B/ui/s$i --only sites --rounds 1 > ui/s$i.out 2>&1 < /dev/null ) & done
wait
for i in 4 5 6; do mkdir -p ui/s$i; ( cs --pin $((12+i)),$((32+i)) --workdir $B/ui/s$i --only sites --rounds 1 > ui/s$i.out 2>&1 < /dev/null ) & done
wait; touch ui.done
