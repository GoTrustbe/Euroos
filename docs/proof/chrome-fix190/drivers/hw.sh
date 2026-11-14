#!/bin/bash
# hw.sh tag img pins n : n sequential hwprobe legs, serial logs kept under q35/tag-i/
cd /srv/euroos/fix190-runs
tag=$1; im=$2; pins=$3; n=${4:-1}
for i in $(seq 1 $n); do
  d=q35/$tag-$i; mkdir -p $d
  cp --sparse=always img/$im.img $d/disk.raw
  TMPDIR=$PWD/$d EK_RESTRICT=1 EK_ACCEL=kvm EK_BOOT_DEADLINE=900 taskset -c $pins python3 scripts/run-metal-matrix.py $PWD/$d/disk.raw --legs hwprobe > $d/out.txt 2>&1
  rm -f $d/disk.raw
  mv $d/ek-matrix-*/hwprobe.serial.log $d/serial.log 2>/dev/null; rm -rf $d/ek-matrix-*
done
touch q35/$tag.done
