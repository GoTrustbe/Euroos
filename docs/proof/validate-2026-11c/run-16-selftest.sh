#!/bin/bash
# .16 (TCG), self-test image of the validated commit: the ten matrix boots, markers and
# the comparison against docs/proof/linux-rand-2026-11 and integ-2026-11b, the megaraid
# proof with and without IOMMU, then the CI q35 matrix.
# Own work directory; every QEMU is stopped by its own PID (boot.sh, the harnesses).
R=/opt/euroos/validate-runs; W=/opt/euroos/wt-chrome
IMG=$R/build/st.img
P=$W/docs/proof
mkdir -p $R/tcg $R/proof
cp $P/integ-2026-11b/tcg-boot.sh $R/tcg/boot.sh
grep -q 'if \[ -n "$PID" \] && \[ "$PID" -gt 1 \]' $R/tcg/boot.sh || { echo "boot.sh: no kill guard" > $R/chainA.done; exit 1; }
cp $P/integ-2026-11b/tcg-matrix.sh $R/tcg/matrix.sh
chmod +x $R/tcg/boot.sh $R/tcg/matrix.sh
OUT=$R/tcg/out
cd $R/tcg && ./matrix.sh $IMG $OUT > $R/tcg/matrix.out 2>&1
: > $R/proof/comparison-tcg.txt; : > $R/proof/tcg-summary.txt; : > $R/proof/tcg-newtests.txt
for c in user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g; do
  L=$OUT/$c.log
  /opt/euroos/integ-runs/mkmarkers.sh $L > $R/proof/tcg-$c.markers
  for base in linux-rand-2026-11 integ-2026-11b; do
    echo "== $c (against $base/tcg-$c.markers)" >> $R/proof/comparison-tcg.txt
    python3 $W/scripts/markers-compare.py compare $P/$base/tcg-$c.markers $R/proof/tcg-$c.markers >> $R/proof/comparison-tcg.txt 2>&1
  done
  d=no; grep -aq "interactive loop started" $L && d=yes
  f=$(grep -aEc "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT|NMI PROBE" $L)
  k=$(grep -a "^\[boot-time\] desktop after" $L | sed -E "s/.*desktop after ([0-9]+) ms.*/\1/" | tail -1)
  lk=$(grep -a "^\[lk\] boot-total" $L | tail -1 | cut -c1-110)
  es=$(grep -ac "^\[entropy\] CSPRNG seeded" $L)
  echo "$c: desktop=$d fault-lines=$f kernel-to-desktop=$k ms seeded=$es | $lk" >> $R/proof/tcg-summary.txt
  echo "== $c" >> $R/proof/tcg-newtests.txt
  grep -a "^\[rename\] [a-z]* exit=\|^\[rename\] rename over\|^\[rename\] .* FAILED\|^\[rename\] .*not run\|^\[rename\] .*setup mappings\|^\[linux-rand\] getrandom" $L | tr -d '\r' | cut -c1-500 >> $R/proof/tcg-newtests.txt
done
echo "matrix done $(date +%T)" > $R/tcg/matrix.done

# megaraid proof, with and without IOMMU
cp --sparse=always $IMG $R/mr-image.img
TIMEOUT=1800 nice -n 10 bash $W/scripts/megaraid-proof.sh $R/mr-image.img $R/mr-iommu > $R/mr-iommu.out 2>&1
echo "rc=$?" >> $R/mr-iommu.out
NO_IOMMU=1 TIMEOUT=1800 nice -n 10 bash $W/scripts/megaraid-proof.sh $R/mr-image.img $R/mr-noiommu > $R/mr-noiommu.out 2>&1
echo "rc=$?" >> $R/mr-noiommu.out
: > $R/proof/comparison-mr.txt
for k in iommu noiommu; do for b in 1 2; do
  cp $R/mr-$k/boot$b.markers $R/proof/mr-$k-boot$b.markers 2>/dev/null || /opt/euroos/integ-runs/mkmarkers.sh $R/mr-$k/boot$b.log > $R/proof/mr-$k-boot$b.markers
  echo "== mr-$k-boot$b (against integ-2026-11b)" >> $R/proof/comparison-mr.txt
  python3 $W/scripts/markers-compare.py compare $P/integ-2026-11b/mr-$k-boot$b.markers $R/proof/mr-$k-boot$b.markers >> $R/proof/comparison-mr.txt 2>&1
  echo "   fault lines: $(grep -aEc "KERNEL PANIC|GENERAL PROTECTION|NMI PROBE|panicked at" $R/mr-$k/boot$b.log)" >> $R/proof/comparison-mr.txt
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
