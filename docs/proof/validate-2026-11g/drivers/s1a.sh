#!/bin/bash
# S1a: candidate functional lanes on disjoint CPU sets (siblings may overlap: functional only)
B=/srv/euroos/val-runs/g; cd $B; S=$B/scripts; I=$B/img
mkdir -p kvm/m opt ent runs tcg /dev/shm/g11
# KVM boots mem x smp
kl() { SMP=$1 PIN=$2; for MEM in 1G 2G 4G 8G; do ./kboot-hda.sh $I/m-st.img $B/kvm/m k-${MEM}-s$SMP $MEM $SMP $PIN "interactive loop started" 900 > kvm/m/k-${MEM}-s$SMP.res 2>&1; done; }
kl 1 0 & kl 2 2-3 & kl 4 4-7 &
# boot options: self-test boot 2G 2 vCPU, then 3 entropy boots each
( for k in off leg fix id nosplit; do ./kboot-hda.sh $I/m-st-$k.img $B/opt o-$k 2G 2 8-9 "interactive loop started" 900 > opt/o-$k.res 2>&1;
   bash $S/short-boots.sh $I/m-st-$k.img $B/opt e-$k 3 8-9 kvm host "[aslr-exec]" 900 2 >/dev/null 2>&1; done; touch opt.done ) &
# entropy: 20 boots of the default, 4 lanes x 5
for p in 10-11 12-13 14-15 16-17; do bash $S/short-boots.sh $I/m-st.img $B/ent m$p 5 $p kvm host "[aslr-exec]" 900 2 >/dev/null 2>&1 & done
# gui-smoke: default then the five options
( for k in "" -off -leg -fix -id -nosplit; do n=gs${k:--on}; rm -rf runs/$n /dev/shm/g11/$n; mkdir -p /dev/shm/g11/$n; ln -s /dev/shm/g11/$n runs/$n
   python3 $S/gui-smoke.py --image $I/m-pub$k.img --workdir /dev/shm/g11/$n --kvm --smp 4 --mem 4G --pin 18-21 > runs/$n.out 2>&1 < /dev/null; done; touch gs.done ) &
# TCG matrix (candidate): two queues
./tcgq.sh m $I/m-st.img 24-27 user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g &
./tcgq.sh m $I/m-st.img 28-29 user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g &
wait; touch s1a.done
