#!/bin/bash
# hb.sh NAME IMG PINS PORT: inbound hammer while the desktop runs churn proc 8 rounds, sched-stat over them
B=/srv/euroos/d2-runs; N=$1 IMG=$2 PINS=$3 PORT=$4; shift 4
C="httpd on;${PRE:-echo pre};sleepwait 5;sched-stat start"; for i in $(seq 1 ${ROUNDS:-8}); do C="$C;spawn worker 32;churn proc 8"; done; C="$C;sched-stat;httpd status"
rm -rf $B/runs/$N; mkdir -p $B/runs/$N; cd $B/runs/$N
python3 -u $B/scripts/loadtest.py --image $IMG --workdir $B/runs/$N --kvm --smp 4 --pin $PINS --nic e1000e --restrict \
  --mem 16G --disks 8G --hostfwd $PORT --hammer 4 --scenario cmds --cmds "$C" --boot-timeout 900 "$@" > $B/runs/$N.out 2>&1 < /dev/null
