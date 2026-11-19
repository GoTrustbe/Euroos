#!/bin/bash
B=/srv/euroos/val-runs/g; cd $B; I=$B/img
mkdir -p /dev/shm/g11/btq fair runs
: > bt-quiet.txt; for i in $(seq 1 12); do for t in m b0; do python3 scripts/boot-time.py $t-$i $I/$t-pub.img /dev/shm/g11/btq --pin 8-9 --smp 2 --mem 2G 2>&1 | tail -n 1 >> bt-quiet.txt; rm -rf /dev/shm/g11/btq/$t-$i.d; done; done; touch bt.done
./klat.sh 6; touch klat.ok
for t in m b0; do for n in $(seq 1 10); do N=$t-$n; rm -rf /dev/shm/g11/fair/$N; mkdir -p /dev/shm/g11/fair/$N
  (cd /dev/shm/g11/fair/$N; python3 -u $B/scripts/loadtest.py --image $B/img/$t-st.img --workdir $PWD --kvm --smp 4 --pin 12-15 --mem 4G --nic e1000e --restrict --scenario cmds --cmds "hogtest 5 10 8" --boot-timeout 900 > out.txt 2>&1)
  grep -a "^\[fair\]" /dev/shm/g11/fair/$N/loadtest-serial-*.log > fair/$N.txt; rm -rf /dev/shm/g11/fair/$N; done; done
touch s3b.done
