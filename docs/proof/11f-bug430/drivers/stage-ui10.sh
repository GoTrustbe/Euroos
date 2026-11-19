#!/bin/bash
# stage-ui.sh IMGPUB IMGBR TAG GSPIN S1PIN S2PIN: gui-smoke (public image, 4 vCPU 4 GiB), Chromium 5 starts and one sites round
B=/srv/euroos/integ2-runs/m430; cd $B; S=$B/scripts; P=/srv/euroos/chrome-runs
PUB=$1 BR=$2 T=$3 GP=$4 P1=$5 P2=$6
shm() { rm -rf runs/$1 /dev/shm/m430/$1; mkdir -p /dev/shm/m430/$1 runs; ln -s /dev/shm/m430/$1 runs/$1; }
cs() { python3 $S/chrome-smoke.py --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --smp 2 --image $B/img/$BR.img "$@"; }
mkdir -p /dev/shm/m430 runs; shm gs-$T
python3 $S/gui-smoke.py --image $B/img/$PUB.img --workdir /dev/shm/m430/gs-$T --kvm --smp 4 --mem 4G --pin $GP > runs/gs-$T.out 2>&1 < /dev/null &
( shm starts-$T; cs --pin $P1 --workdir /dev/shm/m430/starts-$T --starts 10 > runs/starts-$T.out 2>&1 < /dev/null ) &
( shm sites-$T; cs --pin $P2 --workdir /dev/shm/m430/sites-$T --only sites --rounds 1 > runs/sites-$T.out 2>&1 < /dev/null ) &
wait; touch runs/ui-$T.done
