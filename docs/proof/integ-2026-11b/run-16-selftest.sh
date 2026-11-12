#!/bin/bash
# .16 (TCG), self-test image: the ten matrix boots, markers and comparisons, megaraid proof, CI q35 matrix.
R=/opt/euroos/integ2-runs; W=/opt/euroos/wt-integ2
IMG=$R/build/st5.img
P=$W/docs/proof
mkdir -p $R/tcg $R/proof
sed 's/^kill \$PID 2>\/dev\/null; wait \$PID 2>\/dev\/null$/if [ -n "$PID" ] \&\& [ "$PID" -gt 1 ]; then kill "$PID" 2>\/dev\/null; wait "$PID" 2>\/dev\/null; fi/' $P/kdebug-2026-11/tcg-boot.sh > $R/tcg/boot.sh
grep -q 'if \[ -n "$PID" \] && \[ "$PID" -gt 1 \]' $R/tcg/boot.sh || { echo "boot.sh: kill guard not applied" > $R/chainA.done; exit 1; }
cp $P/kdebug-2026-11/tcg-matrix.sh $R/tcg/matrix.sh
chmod +x $R/tcg/boot.sh $R/tcg/matrix.sh
OUT=$R/tcg/out
cd $R/tcg && ./matrix.sh $IMG $OUT > $R/tcg/matrix.out 2>&1
: > $R/proof/comparison-tcg.txt; : > $R/proof/tcg-summary.txt; : > $R/proof/tcg-newtests.txt
for c in user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g; do
  L=$OUT/$c.log
  /opt/euroos/integ-runs/mkmarkers.sh $L > $R/proof/tcg-$c.markers
  for base in integ-2026-11 kdebug-2026-11 uidebug-2026-11; do
    echo "== $c (against $base/tcg-$c.markers)" >> $R/proof/comparison-tcg.txt
    python3 $W/scripts/markers-compare.py compare $P/$base/tcg-$c.markers $R/proof/tcg-$c.markers >> $R/proof/comparison-tcg.txt 2>&1
  done
  d=no; grep -aq "interactive loop started" $L && d=yes
  f=$(grep -aEc "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT|NMI PROBE" $L)
  k=$(grep -a "^\[boot-time\] desktop after" $L | sed -E "s/.*desktop after ([0-9]+) ms.*/\1/" | tail -1)
  ls=$(grep -a "^\[boot-time\] phase lockscreen" $L | sed -E "s/.*lockscreen: ([0-9.]+) ms.*/\1/" | tail -1)
  lk=$(grep -a "^\[lk\] boot-total" $L | tail -1 | cut -c1-110)
  echo "$c: desktop=$d fault-lines=$f kernel-to-desktop=$k ms lockscreen=$ls ms | $lk" >> $R/proof/tcg-summary.txt
  echo "== $c" >> $R/proof/tcg-newtests.txt
  for t in entropy 3d8 3d7 3d3 bug063 fdspace bug053 ag-lock altgr disks; do grep -a "^\[$t\]" $L | tr -d '\r' | cut -c1-700 >> $R/proof/tcg-newtests.txt; done
done
echo "matrix done $(date +%T)" > $R/tcg/matrix.done

# megaraid proof, with and without IOMMU
cp --sparse=always $IMG $R/mr-image.img
TIMEOUT=1800 nice -n 10 bash $W/scripts/megaraid-proof.sh $R/mr-image.img $R/mr-iommu > $R/mr-iommu.out 2>&1
NO_IOMMU=1 TIMEOUT=1800 nice -n 10 bash $W/scripts/megaraid-proof.sh $R/mr-image.img $R/mr-noiommu > $R/mr-noiommu.out 2>&1
: > $R/proof/comparison-mr.txt
for k in iommu noiommu; do for b in 1 2; do
  cp $R/mr-$k/boot$b.markers $R/proof/mr-$k-boot$b.markers 2>/dev/null || /opt/euroos/integ-runs/mkmarkers.sh $R/mr-$k/boot$b.log > $R/proof/mr-$k-boot$b.markers
  for base in integ-2026-11 kdebug-2026-11; do
    echo "== mr-$k-boot$b (against $base)" >> $R/proof/comparison-mr.txt
    python3 $W/scripts/markers-compare.py compare $P/$base/mr-$k-boot$b.markers $R/proof/mr-$k-boot$b.markers >> $R/proof/comparison-mr.txt 2>&1
  done
  echo "   fault lines: $(grep -aEc "KERNEL PANIC|GENERAL PROTECTION|NMI PROBE|panicked at" $R/mr-$k/boot$b.log); $(grep -a "^\[entropy\]" $R/mr-$k/boot$b.log | tr -d '\r' | cut -c1-60)" >> $R/proof/comparison-mr.txt
done; cp $R/mr-$k/host-check.txt $R/proof/mr-$k-host-check.txt 2>/dev/null; done
rm -f $R/mr-image.img $R/mr-*/boot.img $R/mr-*/ld*.raw
echo "mr done $(date +%T)" > $R/mr.done

# CI q35 matrix
cd $W
mkdir -p $R/tmp
cp --sparse=always $IMG $R/q35-image.img
TMPDIR=$R/tmp EK_BOOT_DEADLINE=900 nice -n 10 python3 scripts/run-metal-matrix.py $R/q35-image.img --legs base,nvme,ahci,e1000e,hda,usb,usbhub,usbaudio,usbnet,scan,printer,power,hwprobe > $R/proof/q35-matrix.txt 2>&1
echo "rc=$?" >> $R/proof/q35-matrix.txt
rm -f $R/q35-image.img
echo "done $(date +%T)" > $R/chainA.done
