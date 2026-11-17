#!/bin/bash
# stage-ui.sh: gui-smoke (public image, 4 vCPU, 4 GiB), Chromium 5 starts and one sites round (public image + ui.tour=off,
# update.check=off: user-net, no hostfwd). Merge image only.
B=/srv/euroos/aslr7-runs; cd $B; S=$B/scripts; P=/srv/euroos/chrome-runs
cp img/m-pub.img img/m-br.img
printf "ui.tour=off\nupdate.check=off\n" > /tmp/aslr7.conf
mcopy -o -i img/m-br.img /tmp/aslr7.conf ::/EFI/BOOT/euroos-boot.conf; rm -f /tmp/aslr7.conf
shm() { rm -rf runs/$1 /dev/shm/a7/$1; mkdir -p /dev/shm/a7/$1 runs; ln -s /dev/shm/a7/$1 runs/$1; }
cs() { python3 $S/chrome-smoke.py --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --smp 2 --image $B/img/m-br.img "$@"; }
mkdir -p /dev/shm/a7 runs gs
python3 $S/gui-smoke.py --image $B/img/m-pub.img --workdir $B/gs --kvm --smp 4 --mem 4G --pin 8-11 > runs/gs.out 2>&1 < /dev/null &
( shm starts; cs --pin 12,32 --workdir /dev/shm/a7/starts --starts 5 > runs/starts.out 2>&1 < /dev/null ) &
( shm sites; cs --pin 13,33 --workdir /dev/shm/a7/sites --only sites --rounds 1 > runs/sites.out 2>&1 < /dev/null ) &
wait; touch ui.done
