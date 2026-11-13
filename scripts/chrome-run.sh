#!/usr/bin/env bash
# chrome-run.sh NAME PINS [chrome-smoke args...]   (R630) run chrome-smoke.py in the background,
# own work directory runs/NAME, log runs/NAME.out. Env: RUNS (default /srv/euroos/harness-runs),
# IMG (public image, update.check=off), PACK, NSS. User-mode network without restrict=on
# (browser runs only, no hostfwd to the outside); the guest reaches this host at 10.0.2.2.
B=${RUNS:-/srv/euroos/harness-runs}
IMG=${IMG:-$B/img/pub.img}
PACK=${PACK:-/srv/euroos/chrome-runs/chrome-pack2.img}
NSS=${NSS:-/srv/euroos/chrome-runs/nss-pack-vid.img}
N=$1; PINS=$2; shift 2
rm -rf $B/runs/$N; mkdir -p $B/runs
cd $B && (setsid nohup python3 $B/scripts/chrome-smoke.py --image $IMG --pack $PACK --nss $NSS \
  --kvm --pin $PINS --workdir $B/runs/$N "$@" > $B/runs/$N.out 2>&1 < /dev/null &)
echo started $N
