#!/usr/bin/env bash
# Two queues of TCG boots, each on its own cores. usage: matrix.sh IMG OUTDIR
IMG=$1 OUT=$2
mkdir -p "$OUT"
qa() { for r in "user-smp4-iommu 512M 4 1 user" "user-smp4-noiommu 512M 4 0 user" "kernel-smp4 512M 4 0 kernel" "storage-smp4-iommu-1g 1G 4 1 storage" "combined-smp4-iommu-1g 1G 4 1 combined"; do
  set -- $r; PIN=8-11 ./boot.sh $1 $2 $3 $4 $5 "$IMG" "$OUT" >> "$OUT/queue-a.txt" 2>&1; done; }
qb() { for r in "user-smp1-iommu 512M 1 1 user" "user-smp1-noiommu 512M 1 0 user" "kernel-smp1 512M 1 0 kernel" "storage-smp1-iommu-1g 1G 1 1 storage" "combined-smp1-iommu-1g 1G 1 1 combined"; do
  set -- $r; PIN=12-15 ./boot.sh $1 $2 $3 $4 $5 "$IMG" "$OUT" >> "$OUT/queue-b.txt" 2>&1; done; }
qa & qb & wait
echo "matrix done $(date +%T)" >> "$OUT/queue-a.txt"
