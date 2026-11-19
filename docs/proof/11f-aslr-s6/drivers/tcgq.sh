#!/bin/bash
# tcgq.sh TAG IMG PINS CONFIG...: TCG legs one after the other (tcgboot.sh, S4 driver)
TAG=$1 IMG=$2 PINS=$3; shift 3
B=/srv/euroos/aslr6-runs; mkdir -p $B/tcg/$TAG
for c in "$@"; do
  KIND=${c%%-*}; SMP=$(echo $c | sed -n "s/.*-smp\([0-9]*\).*/\1/p"); IOMMU=0; echo $c | grep -q -- "-iommu" && IOMMU=1
  MEM=2G; echo $c | grep -q -- "-1g" && MEM=1G
  $B/tcgboot.sh $c $MEM $SMP $IOMMU $KIND $IMG $B/tcg/$TAG $PINS >> $B/tcg/$TAG/queue.txt 2>&1
done
