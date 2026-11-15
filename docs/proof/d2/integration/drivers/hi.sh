#!/bin/bash
# hi.sh NAME IMG PINS PORT: inbound hammer with an idle desktop, sched-stat over 60 s
B=/srv/euroos/d2-runs; N=$1 IMG=$2 PINS=$3 PORT=$4; shift 4
rm -rf $B/runs/$N; mkdir -p $B/runs/$N; cd $B/runs/$N
python3 -u $B/scripts/loadtest.py --image $IMG --workdir $B/runs/$N --kvm --smp 4 --pin $PINS --nic e1000e --restrict \
  --mem 4G --hostfwd $PORT --hammer 4 --scenario cmds --cmds "httpd on;sleepwait 5;sched-stat start;sleepwait 60;sched-stat;httpd status;netstat" --boot-timeout 900 "$@" > $B/runs/$N.out 2>&1 < /dev/null
