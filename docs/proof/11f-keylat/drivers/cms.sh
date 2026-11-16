#!/usr/bin/env bash
# cms.sh NAME IMG PINS [args...]: chrome-smoke with the work dir on tmpfs (/dev/shm/11fa/NAME), team 11f-A
B=/srv/euroos/11fa-runs
N=$1; IMG=$2; PINS=$3; shift 3
S=${SCRIPTS:-scripts}
rm -rf $B/runs/$N /dev/shm/11fa/$N; mkdir -p /dev/shm/11fa/$N; ln -s /dev/shm/11fa/$N $B/runs/$N
cd $B && (setsid nohup python3 $S/chrome-smoke.py --image $IMG --pack /srv/euroos/chrome-runs/chrome-pack2.img --nss /srv/euroos/chrome-runs/nss-pack-vid.img \
  --kvm --pin $PINS --workdir /dev/shm/11fa/$N "$@" > $B/runs/$N.out 2>&1 < /dev/null &)
echo started $N
