#!/bin/bash
# collect.sh: text results into out/ (no images, no raw serial logs)
B=/srv/euroos/mem3-runs; S=$B/scripts; O=$B/out; F=/srv/euroos/final-runs; cd $B; mkdir -p $O
mm() { grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" "$1" | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]"; }
KEY="^\[memt\]|^\[cow\]|^\[syncsock\]|^\[many-threads\]|^\[churn-forkthreads\]|^\[fdsem\]|^\[pcmch\]|^\[churn-fork\]|^\[robust\]|^\[bug053\]"
{ echo "# KVM self-test boots of st-f3 (9c30378), R630, 2 vCPU, -nic none, intel-hda (kboot-hda.sh); 1, 2, 4, 8 GiB"; cat kvm/k3-*.res
  for m in 1 2 4 8; do echo; echo "== ${m} GiB"; grep -aE "$KEY" kvm/k3-${m}g.log | cut -c1-330; echo "-- FAILED lines:"; grep -a "^\[" kvm/k3-${m}g.log | grep -a FAILED | cut -c1-200; echo "-- crash lines: $(grep -aEc 'KERNEL PANIC|GENERAL PROTECTION|\[idt\] PAGE FAULT|DOUBLE FAULT|panicked at|WATCHDOG|NMI PROBE|timer-probe\] FROZEN' kvm/k3-${m}g.log)"; done
} > $O/kvm-selftest-boots.txt
{ echo "# markers-compare.py: KVM self-test boots of validate-2026-11d (final-runs/kvm) -> st-f3"
  for m in 1 2 4 8; do echo; echo "== ${m} GiB"; python3 $S/markers-compare.py compare $F/kvm/k-${m}g.log kvm/k3-${m}g.log; done
} > $O/comparison-kvm.txt
CF="user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g"
{ echo "# TCG matrix, R630, restrict=on, st-f3"; cat tcg/f3/queue.txt
  for c in $CF; do L=tcg/f3/$c.log; echo "$c: desktop=$(grep -ac "interactive loop started" $L) watchdog=$(grep -ac "WATCHDOG TRIPPED" $L) nmi=$(grep -ac "NMI PROBE" $L) frozen=$(grep -ac "timer-probe\] FROZEN" $L) fault-lines=$(grep -aEc "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT" $L) FAILED-lines=$(grep -a "^\[[a-z0-9][a-z0-9_./-]*\]" $L | grep -ac FAILED) | $(grep -a "^\[memt\] memory self-test" $L | cut -c1-80)"; done
} > $O/tcg-summary.txt
{ echo "# markers-compare.py: TCG matrix of validate-2026-11d (final-runs/tcg/f) -> st-f3"; for c in $CF; do echo; echo "== $c"; mm $F/tcg/f/$c.log > /tmp/m3a; mm tcg/f3/$c.log > /tmp/m3b; python3 $S/markers-compare.py compare /tmp/m3a /tmp/m3b; done; } > $O/comparison-tcg.txt
{ echo "# gui-smoke, plain public image plain-f2 (d29ff3d), KVM 4 GiB smp 4"; head -n 120 runs/gs-pub/report.txt; } > $O/gui-smoke-public.txt
for n in sites3 interact; do { echo "# chrome-smoke --only $n, public image pubo-f2 (ui.tour=off;update.check=off), KVM, 2 vCPU"; grep -a "chrome-smoke\] \(PASS\|FAIL\)\|\] FAIL" runs/$n.out | cut -c1-400; grep -a "\] ok" runs/$n.out | cut -c1-200; } > $O/chrome-$n.txt; done
grep -h "WORSE" $O/comparison-*.txt | sort | uniq -c
