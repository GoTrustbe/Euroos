#!/bin/bash
B=/srv/euroos/aslr5-runs; cd $B; P=/srv/euroos/chrome-runs
./kboot-hda.sh img/b-ce6ca19-st.img kvm-merge ce6-2g 2G 2 2-3 "interactive loop started" 900 > kvm-merge/ce6-2g.res 2>&1 &
./kboot-hda.sh img/b-9c4d420-st.img kvm-merge 9c4-2g 2G 2 4-5 "interactive loop started" 900 > kvm-merge/9c4-2g.res 2>&1 &
mkdir -p /dev/shm/a5/mstart; rm -rf /dev/shm/a5/mstart/*
python3 scripts/chrome-smoke.py --image img/m-br.img --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin 6-7 --workdir /dev/shm/a5/mstart --starts 1 --smp 2 > kvm-merge/chrome-start.out 2>&1 < /dev/null
cp /dev/shm/a5/mstart/*.log kvm-merge/ 2>/dev/null
wait
for b in ce6 9c4; do echo "## markers-compare $b -> merge, 2G"; python3 scripts/markers-compare.py compare kvm-merge/$b-2g.log kvm-merge/m-2g.log; done > kvm-merge/compare.txt
touch kvm-merge/done
