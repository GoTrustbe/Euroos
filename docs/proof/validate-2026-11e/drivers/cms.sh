#!/usr/bin/env bash
# cms.sh NAME IMG PINS [args...]: cm.sh with the work directory on tmpfs (/dev/shm/val/NAME, linked from runs/NAME):
# the screendumps of the harness are QEMU writes under its device lock, and on the busy disk they stalled the vCPUs
B=/srv/euroos/val-runs
N=$1; IMG=$2; PINS=$3; shift 3
rm -rf $B/runs/$N /dev/shm/val/$N; mkdir -p /dev/shm/val/$N; ln -s /dev/shm/val/$N $B/runs/$N
cd $B && (setsid nohup python3 scripts/chrome-smoke.py --image $IMG --pack /srv/euroos/chrome-runs/chrome-pack2.img --nss /srv/euroos/chrome-runs/nss-pack-vid.img \
  --kvm --pin $PINS --workdir /dev/shm/val/$N "$@" > $B/runs/$N.out 2>&1 < /dev/null &)
echo started $N
