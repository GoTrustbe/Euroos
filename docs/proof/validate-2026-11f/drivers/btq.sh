#!/bin/bash
B=/srv/euroos/v11f-runs; cd $B; mkdir -p /dev/shm/v11f/btq; : > bt-quiet.txt
for i in $(seq 1 12); do for t in m b0; do
  python3 scripts/boot-time.py $t-$i img/$t-pub.img /dev/shm/v11f/btq --pin 8-9 --smp 2 --mem 2G 2>&1 | tail -1 >> bt-quiet.txt; rm -rf /dev/shm/v11f/btq/$t-$i.d
done; done
touch btq.done
./cms.sh sites-smp img/m-brsmp.img 4-7 --only sites --rounds 3 --smp 4
