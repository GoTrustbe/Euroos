#!/bin/bash
# TCG matrix, 10 legs (tcgboot.sh of S5: q35, qemu64,+smep,+smap, e1000e restrict=on), image B
B=/srv/euroos/b380-runs; cd $B; mkdir -p kvm30
./tcgq.sh b $B/img/b-st.img 16,17,36,37 user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g &
./tcgq.sh b $B/img/b-st.img 18,38 user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g &
( for i in 1 2 3 4 5 6 7 8 9 10; do ./loadrun.sh 39 ./kboot.sh $B/img/b-st.img $B/kvm30 l1-$i 2G 1 19 "child has a vDSO" 900 > $B/kvm30/l1-$i.res 2>&1; done ) &
wait; touch tcg.done
