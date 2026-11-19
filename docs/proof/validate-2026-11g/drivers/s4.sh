#!/bin/bash
# S4: loadtest soaks of candidate and reference side by side (1 h, same load, siblings idle), [fair] 10 boots each beside it, update path
B=/srv/euroos/val-runs/g; cd $B; S=$B/scripts; I=$B/img
mkdir -p soak2/m soak2/b0 fair /dev/shm/g11/fair
sk() { T=$1 PIN=$2 PORT=$3; ( cd soak2/$T && python3 -u $S/loadtest.py --image $I/$T-st.img --workdir $B/soak2/$T/w --kvm --smp 4 --pin $PIN --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd $PORT --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 3900 --boot-timeout 900 > out.txt 2>&1 < /dev/null; rm -rf $B/soak2/$T/w/*.img $B/soak2/$T/w/*.raw ); }
sk m 0-3 18621 &
sk b0 4-7 18622 &
( for t in m b0; do for n in $(seq 1 10); do N=$t-$n; rm -rf /dev/shm/g11/fair/$N; mkdir -p /dev/shm/g11/fair/$N
  (cd /dev/shm/g11/fair/$N; python3 -u $B/scripts/loadtest.py --image $I/$t-st.img --workdir $PWD --kvm --smp 4 --pin 12-15 --mem 4G --nic e1000e --restrict --scenario cmds --cmds "hogtest 5 10 8" --boot-timeout 900 > out.txt 2>&1)
  grep -a "^\[fair\]" /dev/shm/g11/fair/$N/loadtest-serial-*.log > fair/$N.txt; rm -rf /dev/shm/g11/fair/$N; done; done; touch fair.done ) &
PIN=30-33 ./upd.sh > upd.out 2>&1 &
wait; touch s4.done
