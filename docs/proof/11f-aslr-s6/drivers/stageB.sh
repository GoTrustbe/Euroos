#!/bin/bash
# stageB.sh: quiet part (no TCG, at most four guests): sites, interaction, soaks; vdso-mono reruns. CPUs 0-15/20-35.
B=/srv/euroos/aslr6-runs; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs
until [ -f stageA.done ] && [ -f short.done ] && [ -f installs.done ]; do sleep 30; done
shm() { rm -rf runs/$1 /dev/shm/a6/$1; mkdir -p /dev/shm/a6/$1 runs; ln -s /dev/shm/a6/$1 runs/$1; }
cs() { python3 $S/chrome-smoke.py --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --smp 2 "$@"; }
sites() { N=$1 IMG=$2 PIN=$3 R=$4; shm $N; cs --image $IMG --pin $PIN --workdir /dev/shm/a6/$N --only sites --rounds $R > runs/$N.out 2>&1 < /dev/null; }
soak() { N=$1 IMG=$2 PIN=$3 PORT=$4; shm $N
  python3 $S/chrome-mem-soak.py --image $IMG --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin $PIN --smp 2 --mem 8192M --workdir /dev/shm/a6/$N --no-video --free-video-seconds 0 --hours 1 --local-video-dir $B/vid --local-seconds 60 --local-port $PORT > runs/$N.out 2>&1 < /dev/null; }
mkdir -p /dev/shm/a6 kvmq
# B1: sites A/B, vdso-mono reruns (aslr=off 2 GiB, 3 each)
sites sites-on $I/s6-br.img 0-1 3 &
sites sites-base $I/base-br.img 2-3 3 &
( for i in 1 2 3; do ./kboot-hda.sh $I/s6-st-off.img $B/kvmq s6off-2G-$i 2G 2 8-9 "interactive loop started" 900 > kvmq/s6off-2G-$i.res 2>&1; done ) &
( for i in 1 2 3; do ./kboot-hda.sh $I/base-st-off.img $B/kvmq baseoff-2G-$i 2G 2 10-11 "interactive loop started" 900 > kvmq/baseoff-2G-$i.res 2>&1; done ) &
wait; touch B1.done
# B2: interaction (on), one sites round with off and legacy
( N=interact-on; shm $N; cs --image $I/s6-br.img --pin 0-1 --workdir /dev/shm/a6/$N --only interact > runs/$N.out 2>&1 < /dev/null ) &
sites sites-off $I/s6-br-off.img 2-3 1 &
sites sites-leg $I/s6-br-leg.img 4-5 1 &
wait; touch B2.done
# B3: chrome-mem-soak 1 h on and base side by side, loadtest soak 1 h on
soak soak-on $I/s6-br.img 0-1 18801 &
soak soak-base $I/base-br.img 2-3 18802 &
( mkdir -p soak-lt; cd soak-lt && python3 -u $S/loadtest.py --image $I/s6-st.img --workdir $B/soak-lt/w --kvm --smp 4 --pin 4-7 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18611 --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 3900 --boot-timeout 900 > out.txt 2>&1 < /dev/null; rm -rf $B/soak-lt/w/*.img $B/soak-lt/w/*.raw ) &
wait; touch B3.done stageB.done
