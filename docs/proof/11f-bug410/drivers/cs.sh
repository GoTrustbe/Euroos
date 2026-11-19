#!/bin/bash
# cs.sh PINS WORKDIR-NAME IMAGE chrome-smoke args...
B=/srv/euroos/integ2-b410-runs; P=/srv/euroos/chrome-runs; PIN=$1 N=$2 IMG=$3; shift 3
mkdir -p /dev/shm/integ2b410/$N; ln -sfn /dev/shm/integ2b410/$N $B/runs/$N
python3 $B/scripts/chrome-smoke.py --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --smp 2 --image $B/img/$IMG.img --pin $PIN --workdir /dev/shm/integ2b410/$N "$@" > $B/runs/$N.out 2>&1 < /dev/null
