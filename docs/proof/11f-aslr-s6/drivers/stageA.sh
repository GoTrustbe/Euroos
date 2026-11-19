#!/bin/bash
# stageA.sh: ASLR S6 regression, heavy part (no Chromium sites, no soaks). s6 = 831894f, base = 7b2eea6 (S5).
B=/srv/euroos/aslr6-runs; cd $B; S=$B/scripts; I=$B/img; P=/srv/euroos/chrome-runs
shm() { rm -rf runs/$1 /dev/shm/a6/$1; mkdir -p /dev/shm/a6/$1 runs; ln -s /dev/shm/a6/$1 runs/$1; }
cs() { python3 $S/chrome-smoke.py --pack $P/chrome-pack2.img --nss $P/nss-pack-vid.img --kvm --smp 2 "$@"; }
chromestarts() { M=$1 IMG=$2 PIN=$3
  N=starts20-$M; shm $N; cs --image $IMG --pin $PIN --workdir /dev/shm/a6/$N --starts 20 > runs/$N.out 2>&1 < /dev/null
  touch chrome-$M.done; }
kvmlane() { PIN=$1; shift
  for job in "$@"; do set -- $(echo $job | tr : " "); T=$1 MEM=$2; mkdir -p kvm/$T
    ./kboot-hda.sh $I/$T.img $B/kvm/$T k-$MEM $MEM 2 $PIN "interactive loop started" 900 > kvm/$T/k-$MEM.res 2>&1
  done; }
kvmall() {
  kvmlane 8-9 s6-st:1G s6-st:2G s6-st:4G s6-st:8G s6-st-fix:2G s6-st-nosplit:2G &
  kvmlane 10-11 base-st:1G base-st:2G base-st:4G base-st:8G base-st-fix:2G base-st-id:2G &
  kvmlane 12-13 s6-st-off:1G s6-st-off:2G s6-st-off:4G s6-st-off:8G s6-st-leg:1G s6-st-leg:2G s6-st-leg:4G s6-st-leg:8G s6-st-id:2G &
  kvmlane 14-15 base-st-off:1G base-st-off:2G base-st-off:4G base-st-off:8G base-st-leg:1G base-st-leg:2G base-st-leg:4G base-st-leg:8G &
  wait
  for M in on off leg; do IMG=$I/s6-pub.img; [ $M != on ] && IMG=$I/s6-pub-$M.img
    rm -rf runs/gs-$M; mkdir -p runs/gs-$M; python3 $S/gui-smoke.py --image $IMG --workdir $B/runs/gs-$M --kvm --smp 4 --mem 4G --pin 8-11 > runs/gs-$M.out 2>&1 < /dev/null; done
  mkdir -p q35-on && cp $I/s6-st.img q35-on/st.raw && (cd q35-on && EK_RESTRICT=1 EK_ACCEL=kvm taskset -c 12-13 python3 $S/run-metal-matrix.py $B/q35-on/st.raw --legs base,nvme,ahci,e1000e,hda,usb,usbhub,usbaudio,usbnet,hwprobe,scan,nvmeroot,ahciroot,power,printer > matrix.out 2>&1 < /dev/null); rm -f q35-on/st.raw
  touch kvm.done; }
entlane() {
  bash $S/short-boots.sh $I/s6-st.img $B/ent s6a 10 16-17 kvm host "[aslr-exec]" 900 2 > /dev/null 2>&1 &
  bash $S/short-boots.sh $I/s6-st.img $B/ent s6b 10 18-19 kvm host "[aslr-exec]" 900 2 > /dev/null 2>&1 &
  bash $S/short-boots.sh $I/base-st.img $B/ent basea 10 30-31 kvm host "[aslr-exec]" 900 2 > /dev/null 2>&1 &
  bash $S/short-boots.sh $I/base-st.img $B/ent baseb 10 32-33 kvm host "[aslr-exec]" 900 2 > /dev/null 2>&1 &
  wait; touch ent.done; }
tcglane() {
  ./tcgq.sh on $I/s6-st.img 24-27 user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g &
  ./tcgq.sh on $I/s6-st.img 28-29 user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g &
  ./tcgq.sh base $I/base-st.img 36-39 user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g &
  ./tcgq.sh base $I/base-st.img 34-35 user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g &
  wait; touch tcg.done; }
mkdir -p /dev/shm/a6 ent
chromestarts on $I/s6-br.img 0-1 &
chromestarts off $I/s6-br-off.img 2-3 &
chromestarts leg $I/s6-br-leg.img 4-5 &
chromestarts base $I/base-br.img 6-7 &
kvmall &
entlane &
tcglane &
wait
touch stageA.done
