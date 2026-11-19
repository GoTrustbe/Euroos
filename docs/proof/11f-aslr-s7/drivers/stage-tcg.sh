#!/bin/bash
# TCG matrix, 10 legs (tcgboot.sh: q35, qemu64,+smep,+smap, e1000e restrict=on), merge image
B=/srv/euroos/aslr7-runs; cd $B
./tcgq.sh m $B/img/m-st.img 4,5,24,25 user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g &
./tcgq.sh m $B/img/m-st.img 6,26 user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g &
wait; touch tcg.done
