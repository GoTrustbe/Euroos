#!/usr/bin/env bash
# tcgmatrix.sh TAG IMG PINS_A PINS_B: the 10-configuration TCG matrix in two queues
TAG=$1 IMG=$2 PA=$3 PB=$4
cd /srv/euroos/mem3-runs; mkdir -p tcg/$TAG
qa() { for c in user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g; do ./tcgq1.sh $TAG $c $IMG $PA; done; }
qb() { for c in user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g; do ./tcgq1.sh $TAG $c $IMG $PB; done; }
qa & qb & wait
echo "matrix $TAG done $(date +%T)" >> tcg/$TAG/queue.txt
