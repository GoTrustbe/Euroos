#!/bin/bash
cd /srv/euroos/integ2-runs/m430/tcg/m
for f in user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g; do
  L=$f.log; [ -f $L ] || continue
  echo "$f: desktop=$(grep -ac "interactive loop started" $L) watchdog=$(grep -ac "WATCHDOG TRIPPED" $L) nmi=$(grep -ac "NMI PROBE" $L) crash=$(grep -aEc "KERNEL PANIC|GENERAL PROTECTION|DOUBLE FAULT|panicked at" $L) vdso-mono=$(grep -a "^\[vdso-mono\]" $L | grep -c "✓") clock-drift-ok=$(grep -a "^\[clock-drift\]" $L | grep -c -- "-> OK") clock-drift-failed=$(grep -a "^\[clock-drift\]" $L | grep -c FAILED) failed-lines=$(grep -a "^\[[a-z0-9_./-]*\]" $L | grep -c FAILED) swap-irq=$(grep -a "^\[swap-irq\]" $L | grep -c "✓")"
done
