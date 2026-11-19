#!/bin/bash
# rest of final.sh after the harness error (P was overwritten by lane()): Chromium starts + 3-round session, video, loadtest soak
B=/srv/euroos/b400-runs; cd $B; S=$B/scripts; CP=/srv/euroos/chrome-runs
cs() { python3 $S/chrome-smoke.py --pack $CP/chrome-pack2.img --nss $CP/nss-pack-vid.img --kvm --smp 2 --image $B/img/b-br.img "$@"; }
( mkdir -p ui/starts; cs --pin 18,38 --workdir $B/ui/starts --starts 20 > ui/starts.out 2>&1 < /dev/null ) &
( mkdir -p ui/r3; cs --pin 19,39 --workdir $B/ui/r3 --only sites --rounds 3 > ui/r3.out 2>&1 < /dev/null ) &
wait
mkdir -p ui/vid
python3 $S/chrome-mem-soak.py --image $B/img/b-br.img --pack $CP/chrome-pack2.img --nss $CP/nss-pack-vid.img --kvm --pin 18,38 --smp 2 --mem 8192M --workdir $B/ui/vid --no-video --free-video-seconds 0 --hours 0.5 --local-video-dir $B/vid --local-seconds 60 --local-port 18841 > ui/vid.out 2>&1 < /dev/null
touch ui.done
mkdir -p soak; (cd soak && python3 -u $S/loadtest.py --image $B/img/b-st.img --workdir $B/soak/w --kvm --smp 4 --pin 18,19,38,39 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18842 --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 3900 --boot-timeout 900 > out.txt 2>&1 < /dev/null)
touch final.done
