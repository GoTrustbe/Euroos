#!/bin/bash
# after queue1: drift 30 min (A/B pairs), 30 loaded KVM boots, markers boots, TCG matrix, UI, loadtest soak. CPUs 18,19,38,39 only.
B=/srv/euroos/b400-runs; cd $B
until [ -e tf/queue1.done ] && [ -e img/b-pub.img ] && [ -e img/b-br.img ] && [ -e img/base-st.img ]; do sleep 30; done
S=$B/scripts; P=/srv/euroos/chrome-runs
# 1. drift, 30 min, A (475f73c: old clock + uptime diag) and B (fix) at the same time
mkdir -p drift
python3 vdrift.py img/a-st.img drift/rest-a 18,38 2 2G 1800 > drift/rest-a.txt 2>&1 &
python3 vdrift.py img/b-st.img drift/rest-b 19,39 2 2G 1800 > drift/rest-b.txt 2>&1 &
wait
taskset -c 38 sh -c "while :; do :; done" & L1=$!
taskset -c 39 sh -c "while :; do :; done" & L2=$!
python3 vdrift.py img/a-st.img drift/load-a 18 1 2G 1800 > drift/load-a.txt 2>&1 & V1=$!
python3 vdrift.py img/b-st.img drift/load-b 19 1 2G 1800 > drift/load-b.txt 2>&1 & V2=$!
wait $V1 $V2
for p in $L1 $L2; do [ -n "$p" ] && [ "$p" -gt 1 ] && kill $p; done
python3 vdrift.py img/a-st.img drift/tcg-a 18,38 1 2G 1800 tcg > drift/tcg-a.txt 2>&1 &
python3 vdrift.py img/b-st.img drift/tcg-b 19,39 1 2G 1800 tcg > drift/tcg-b.txt 2>&1 &
wait; touch drift.done
# 2. 30 KVM boots of B with busy siblings
mkdir -p kvm30; M="[clock-drift] the clock and the tick count"
for i in $(seq 1 10); do ./loadrun.sh 38 ./kboot.sh img/b-st.img kvm30 l1-$i 2G 1 18 "$M" 900 > kvm30/l1-$i.res 2>&1; done
for i in $(seq 1 10); do ./loadrun.sh 38,39 ./kboot.sh img/b-st.img kvm30 l2-$i 2G 2 18,19 "$M" 900 > kvm30/l2-$i.res 2>&1; done
for i in $(seq 1 10); do ./loadrun.sh 38,39 ./kboot.sh img/b-st.img kvm30 l4-$i 2G 4 18,19,38,39 "$M" 900 > kvm30/l4-$i.res 2>&1; done
# 3. markers boots, B and base (19f7586), quiet
mkdir -p kvmm
lane() { P=$1; shift; for j in "$@"; do set -- $(echo $j | tr : " "); ./kboot.sh img/$1-st.img kvmm $1-$2-$3 $2 $3 $P "interactive loop started" 900 > kvmm/$1-$2-$3.res 2>&1; done; }
lane 18,38 b:1G:1 b:2G:1 b:4G:1 b:1G:2 b:2G:2 b:4G:2 &
lane 19,39 base:1G:1 base:2G:1 base:4G:1 base:1G:2 base:2G:2 base:4G:2 &
wait
lane 18,19,38,39 b:1G:4 b:2G:4 b:4G:4 base:1G:4 base:2G:4 base:4G:4
touch kvm.done
# 4. TCG matrix, 10 legs
./tcgq.sh b $B/img/b-st.img 18,38 user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g &
./tcgq.sh b $B/img/b-st.img 19,39 user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g &
wait; touch tcg.done
# 5. UI
mkdir -p ui/gs
python3 $S/gui-smoke.py --image $B/img/b-pub.img --workdir $B/ui/gs --kvm --smp 4 --mem 4G --pin 18,19,38,39 > ui/gs.out 2>&1 < /dev/null
cs() { python3 $S/chrome-smoke.py --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --smp 2 --image $B/img/b-br.img "$@"; }
( mkdir -p ui/starts; cs --pin 18,38 --workdir $B/ui/starts --starts 20 > ui/starts.out 2>&1 < /dev/null ) &
( mkdir -p ui/r3; cs --pin 19,39 --workdir $B/ui/r3 --only sites --rounds 3 > ui/r3.out 2>&1 < /dev/null ) &
wait
mkdir -p ui/vid
python3 $S/chrome-mem-soak.py --image $B/img/b-br.img --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --pin 18,38 --smp 2 --mem 8192M --workdir $B/ui/vid --no-video --free-video-seconds 0 --hours 0.5 --local-video-dir $B/vid --local-seconds 60 --local-port 18841 > ui/vid.out 2>&1 < /dev/null
touch ui.done
# 6. loadtest soak 1 h
mkdir -p soak; (cd soak && python3 -u $S/loadtest.py --image $B/img/b-st.img --workdir $B/soak/w --kvm --smp 4 --pin 18,19,38,39 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18842 --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 3900 --boot-timeout 900 > out.txt 2>&1 < /dev/null)
touch final.done
