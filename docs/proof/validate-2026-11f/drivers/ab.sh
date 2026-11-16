#!/bin/bash
# ab.sh NAME IMG PINS PORT DURATION [extra]: short loadtest soak with the inbound hammer, runs/NAME
B=/srv/euroos/v11f-runs; N=$1 IMG=$2 PINS=$3 PORT=$4 DUR=$5; shift 5
rm -rf $B/runs/$N; mkdir -p $B/runs/$N; cd $B/runs/$N
python3 $B/scripts/loadtest.py --image $IMG --workdir $B/runs/$N --kvm --smp 4 --pin $PINS --nic e1000e --restrict --nmi-on-hang \
  --mem 16G --disks 8G --host-http --hostfwd $PORT --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration $DUR --boot-timeout 900 "$@" > $B/runs/$N.out 2>&1 < /dev/null
