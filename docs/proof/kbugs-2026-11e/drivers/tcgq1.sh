#!/usr/bin/env bash
# tcgq1.sh TAG CONFIG IMG PINS: one boot of CONFIG, log tcg/TAG/CONFIG.log
TAG=$1 CFG=$2 IMG=$3 PINS=$4
case $CFG in
  user-smp4-iommu) A="512M 4 1 user";; user-smp4-noiommu) A="512M 4 0 user";; kernel-smp4) A="512M 4 0 kernel";;
  storage-smp4-iommu-1g) A="1G 4 1 storage";; combined-smp4-iommu-1g) A="1G 4 1 combined";;
  user-smp1-iommu) A="512M 1 1 user";; user-smp1-noiommu) A="512M 1 0 user";; kernel-smp1) A="512M 1 0 kernel";;
  storage-smp1-iommu-1g) A="1G 1 1 storage";; combined-smp1-iommu-1g) A="1G 1 1 combined";;
esac
./tcgboot.sh $CFG $A $IMG tcg/$TAG $PINS >> tcg/$TAG/queue.txt 2>&1
