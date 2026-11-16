#!/bin/bash
# pw.sh TAG IMG PINS N : power leg alone, KVM, restrict=on
cd /srv/euroos/fix270-runs; tag=$1; im=$2; pins=$3; n=$4
for i in $(seq 1 $n); do d=pw/$tag-$i; mkdir -p $d; cp --sparse=always $im $d/disk.raw
  TMPDIR=$PWD/$d EK_RESTRICT=1 EK_ACCEL=kvm EK_BOOT_DEADLINE=900 taskset -c $pins python3 scripts/run-metal-matrix.py $PWD/$d/disk.raw --legs power > $d/out.txt 2>&1
  rm -f $d/disk.raw; mv $d/ek-matrix-*/power.serial.log $d/serial.log 2>/dev/null; rm -rf $d/ek-matrix-*
  echo "$tag-$i | $(grep -a "\[power\]" $d/out.txt | tail -1) | pbp=$(grep -ac "power button pressed" $d/serial.log) sd=$(grep -ac "shutting down system" $d/serial.log)"; done
