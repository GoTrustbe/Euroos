#!/bin/bash
# S3a: clock drift 30 min (cand/ref, rest/loaded), reference soaks (loadtest 1 h, chrome-mem 1 h), clock-drift repeat boots at 8 GiB 1 vCPU. CPUs 0-19, siblings idle except the busy loops of the loaded drift runs
B=/srv/euroos/val-runs/g; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs
mkdir -p drift soak/b0 /dev/shm/g11 runs kvm/rep
shm() { rm -rf runs/$1 /dev/shm/g11/$1; mkdir -p /dev/shm/g11/$1; ln -s /dev/shm/g11/$1 runs/$1; }
python3 vdrift.py $I/m-st.img drift/m-rest 8,28 2 2G 1800 > drift/m-rest.txt 2>&1 &
python3 vdrift.py $I/b0-st.img drift/b0-rest 2,22 2 2G 1800 > drift/b0-rest.txt 2>&1 &
./loadrun.sh 24 python3 vdrift.py $I/m-st.img drift/m-load 4 1 2G 1800 > drift/m-load.txt 2>&1 &
./loadrun.sh 26 python3 vdrift.py $I/b0-st.img drift/b0-load 6 1 2G 1800 > drift/b0-load.txt 2>&1 &
( cd soak/b0 && python3 -u $S/loadtest.py --image $I/b0-st.img --workdir $B/soak/b0/w --kvm --smp 4 --pin 16-19 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18612 --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 3900 --boot-timeout 900 > out.txt 2>&1 < /dev/null; rm -rf $B/soak/b0/w/*.img $B/soak/b0/w/*.raw ) &
( N=msoak-b0; shm $N; python3 $S/chrome-mem-soak.py --image $I/b0-br.img --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin 10-11 --smp 2 --mem 8192M --workdir /dev/shm/g11/$N --no-video --free-video-seconds 0 --hours 1 --local-video-dir $B/vid --local-seconds 60 --local-port 18804 > runs/$N.out 2>&1 < /dev/null ) &
( for i in 1 2 3 4 5 6 7 8; do for t in m b0; do ./kboot-hda.sh $I/$t-st.img $B/kvm/rep $t-8G-s1-$i 8G 1 12 "interactive loop started" 900 > kvm/rep/$t-8G-s1-$i.res 2>&1; done; done; touch rep.done ) &
wait; touch s3a.done
