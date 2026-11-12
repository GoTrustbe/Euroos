#!/bin/bash
# .16 (TCG), self-test image of the final commit: the ten matrix boots, markers
# and the comparison against docs/proof/integ-2026-11b, then the CI q35 matrix.
# Own work directory; every QEMU is stopped by its own PID (boot.sh, the harness).
R=/opt/euroos/rand-runs; W=/opt/euroos/wt-rand
IMG=$R/img/after-st.img
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
  echo "== $c (against integ-2026-11b/tcg-$c.markers)" >> $R/proof/comparison-tcg.txt
  python3 $W/scripts/markers-compare.py compare $P/integ-2026-11b/tcg-$c.markers $R/proof/tcg-$c.markers >> $R/proof/comparison-tcg.txt 2>&1
  d=no; grep -aq "interactive loop started" $L && d=yes
  f=$(grep -aEc "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT|NMI PROBE" $L)
  k=$(grep -a "^\[boot-time\] desktop after" $L | sed -E "s/.*desktop after ([0-9]+) ms.*/\1/" | tail -1)
  lk=$(grep -a "^\[lk\] boot-total" $L | tail -1 | cut -c1-110)
  es=$(grep -ac "^\[entropy\] CSPRNG seeded" $L); el=$(grep -ac "^\[entropy\]" $L)
  echo "$c: desktop=$d fault-lines=$f kernel-to-desktop=$k ms entropy-lines=$el seeded=$es | $lk" >> $R/proof/tcg-summary.txt
  echo "== $c" >> $R/proof/tcg-newtests.txt
  for t in tpm entropy 3d8 3d7 3d3 ae; do grep -a "^\[$t\]" $L | tr -d '\r' | cut -c1-700 >> $R/proof/tcg-newtests.txt; done
  grep -a "^\[linux-rand\] [a-z#0-9]* exit=\|^\[linux-rand\] getrandom\|^\[linux-rand\] .*randprobe: \(at_random\|stack_guard\|getrandom\|urandom\|random\|closed\|stat\|CHECK\)" $L | tr -d '\r' | cut -c1-900 >> $R/proof/tcg-newtests.txt
done
echo "matrix done $(date +%T)" > $R/tcg/matrix.done

# CI q35 matrix
cd $W
mkdir -p $R/tmp
cp --sparse=always $IMG $R/q35-image.img
TMPDIR=$R/tmp EK_BOOT_DEADLINE=900 nice -n 10 python3 scripts/run-metal-matrix.py $R/q35-image.img --legs base,nvme,ahci,e1000e,hda,usb,usbhub,usbaudio,usbnet,scan,printer,power,hwprobe > $R/proof/q35-matrix.txt 2>&1
echo "rc=$?" >> $R/proof/q35-matrix.txt
rm -f $R/q35-image.img
echo "done $(date +%T)" > $R/chainA.done
