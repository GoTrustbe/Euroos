#!/bin/bash
# nu-gedraaid: 1 h soak B and base side by side, keylat/site-load x3, rest-drift B repeat. CPUs 0-9 and 20-29 only.
B=/srv/euroos/b400-runs; cd $B; S=$B/scripts; CP=/srv/euroos/chrome-runs; mkdir -p run2
soak() { N=$1 IMG=$2 PIN=$3 PORT=$4; mkdir -p run2/soak-$N; (cd run2/soak-$N && python3 -u $S/loadtest.py --image $B/$IMG --workdir $B/run2/soak-$N/w --kvm --smp 4 --pin $PIN --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd $PORT --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 3900 --boot-timeout 900 > out.txt 2>&1 < /dev/null); touch run2/soak-$N.done; }
soak b img/b-st.img 0,1,20,21 18842 &
soak base img/base-st.img 2,3,22,23 18845 &
kl() { N=$1 IMG=$2 PIN=$3; mkdir -p /dev/shm/b400/$N; python3 $S/chrome-smoke.py --image $IMG --pack $CP/chrome-pack2.img --nss $CP/nss-pack-vid.img --kvm --pin $PIN --smp 2 --workdir /dev/shm/b400/$N --only perf > run2/$N.out 2>&1 < /dev/null; rm -rf /dev/shm/b400/$N; }
python3 vdrift.py img/b-st.img run2/drift-rest-b2 6,26 2 2G 1800 > run2/drift-rest-b2.txt 2>&1 &
( for r in 1 2 3; do
  if [ $((r%2)) = 1 ]; then PB=4,24; PA=5,25; else PB=5,25; PA=4,24; fi
  kl kb$r img/b-br.img $PB & kl kbase$r img/base-br.img $PA & wait
done; touch run2/kl.done ) &
wait
touch run2.done
