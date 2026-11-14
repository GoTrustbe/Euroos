#!/usr/bin/env bash
# cm.sh NAME IMG PINS SCRIPT [args...]   run a chrome harness in the background in runs/NAME, log runs/NAME.out
B=/srv/euroos/fix190-runs
N=$1; IMG=$2; PINS=$3; S=$4; shift 4
rm -rf $B/runs/$N; mkdir -p $B/runs/$N
cd $B && (setsid nohup python3 scripts/$S --image $IMG --pack /srv/euroos/chrome-runs/chrome-pack2.img --nss /srv/euroos/chrome-runs/nss-pack-vid.img \
  --kvm --pin $PINS --workdir $B/runs/$N "$@" > $B/runs/$N.out 2>&1 < /dev/null &)
echo started $N
