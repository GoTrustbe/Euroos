#!/bin/bash
# S2: candidate Chromium lanes and soaks. CPUs 0-19 only, siblings 20-39 idle. Image m-br* = public-type image + ui.tour=off;update.check=off(+aslr option)
B=/srv/euroos/val-runs/g; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs
mkdir -p /dev/shm/g11 runs soak
shm() { rm -rf runs/$1 /dev/shm/g11/$1; mkdir -p /dev/shm/g11/$1; ln -s /dev/shm/g11/$1 runs/$1; }
cs() { python3 $S/chrome-smoke.py --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --smp 2 "$@"; }
run() { N=$1 IMG=$2 PIN=$3; shift 3; shm $N; cs --image $IMG --pin $PIN --workdir /dev/shm/g11/$N "$@" > runs/$N.out 2>&1 < /dev/null; }
msoak() { N=$1 IMG=$2 PIN=$3 PORT=$4; shm $N
  python3 $S/chrome-mem-soak.py --image $IMG --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin $PIN --smp 2 --mem 8192M --workdir /dev/shm/g11/$N --no-video --free-video-seconds 0 --hours 1 --local-video-dir $B/vid --local-seconds 60 --local-port $PORT > runs/$N.out 2>&1 < /dev/null; }
# long runs
msoak msoak1 $I/m-br.img 10-11 18801 &
msoak msoak2 $I/m-br.img 12-13 18802 &
msoak msoak3 $I/m-br.img 14-15 18803 &
( mkdir -p soak/m; cd soak/m && python3 -u $S/loadtest.py --image $I/m-st.img --workdir $B/soak/m/w --kvm --smp 4 --pin 16-19 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18611 --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 3900 --boot-timeout 900 > out.txt 2>&1 < /dev/null; rm -rf $B/soak/m/w/*.img $B/soak/m/w/*.raw; touch $B/soak.m.done ) &
# C1: starts, sites x3, interaction, local video, Wikimedia video
( run starts20-m $I/m-br.img 0-1 --starts 20
  run sites3-m $I/m-br.img 0-1 --only sites --rounds 3
  run interact-m $I/m-br.img 0-1 --only interact
  run localvideo-m $I/m-br.img 0-1 --only localvideo --media-dir $B/vid --local-port 18781
  N=wm480; shm $N; python3 $S/wmvideo.py --image $I/m-br.img --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --workdir /dev/shm/g11/$N --pin 0-1 --kvm --smp 2 --mem 6144M --seconds 90 --times 2 --url https://upload.wikimedia.org/wikipedia/commons/transcoded/c/c0/Big_Buck_Bunny_4K.webm/Big_Buck_Bunny_4K.webm.480p.vp9.webm > runs/$N.out 2>&1 < /dev/null
  touch c1.done ) &
# C2-C4: boot options, 5 starts + one sites round, default and five options
optlane() { PIN=$1; shift; for k in "$@"; do run opt-starts-$k $I/m-br${k/#on/}.img $PIN --starts 5; run opt-sites-$k $I/m-br${k/#on/}.img $PIN --only sites --rounds 1; done; touch opt-$PIN.done; }
for k in on off leg fix id nosplit; do :; done
optlane2() { PIN=$1; shift; for k in "$@"; do IMG=$I/m-br.img; [ $k != on ] && IMG=$I/m-br-$k.img; run opt-starts-$k $IMG $PIN --starts 5; run opt-sites-$k $IMG $PIN --only sites --rounds 1; done; touch opt-$PIN.done; }
optlane2 2-3 on off &
optlane2 4-5 leg fix &
optlane2 6-7 id nosplit &
# C5: reference Chromium, 20 starts and 3 sites rounds
( run starts20-b0 $I/b0-br.img 8-9 --starts 20; run sites3-b0 $I/b0-br.img 8-9 --only sites --rounds 3; touch c5.done ) &
wait; touch s2.done
