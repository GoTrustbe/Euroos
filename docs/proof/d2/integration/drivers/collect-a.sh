#!/bin/bash
# collect-a.sh: step A (integration) text results into out-a/
B=/srv/euroos/d2-runs; S=$B/scripts; O=$B/out-a; cd $B; rm -rf $O; mkdir -p $O
KEY="^\[memt\]|^\[cow|^\[aslr\]|^\[rename|^\[linux-rand\]|^\[nmi-bound\]|^\[fair|^\[many-threads\]|^\[churn-fork|^\[fdsem\]|^\[hda\]|^\[keyfocus\]|^\[lk\] boot-total|^\[smp\]|^\[ap-tss\]|^\[kwork\]"
{ echo "# KVM self-test boots of the integration merge (b6eb05d), R630, 2 vCPU, -nic none, intel-hda (kboot-hda.sh); 1, 2, 4, 8 GiB"; cat kvm/m3/*.res
  for m in 1 2 4 8; do echo; echo "== ${m} GiB"; grep -aE "$KEY" kvm/m3/k-${m}g.log | cut -c1-240; echo "-- FAILED lines:"; grep -a "^\[" kvm/m3/k-${m}g.log | grep -a FAILED | cut -c1-200; echo "-- crash lines: $(grep -aEc "KERNEL PANIC|GENERAL PROTECTION|\[idt\] PAGE FAULT|DOUBLE FAULT|panicked at|WATCHDOG|NMI PROBE|timer-probe\] FROZEN" kvm/m3/k-${m}g.log) lock inversions: $(grep -ac "LOCK ORDER INVERSION" kvm/m3/k-${m}g.log)"; done; } > $O/kvm-selftest-boots.txt
{ echo "# markers-compare.py, KVM self-test boots: the proof boots of each team -> integration merge b6eb05d"
  for base in final-runs/kvm:validate-2026-11d sched-runs/kvm:sched-d1 kbugs-runs/kvm:kbugs-2026-11e small-runs/final/kvm:small-2026-11e mem3-runs/kvm:mem-2026-11e; do d=${base%%:*}; n=${base##*:}
    for m in 1 2 4 8; do echo; echo "== $n ${m} GiB"; python3 $S/markers-compare.py compare ../$d/k-${m}g.log kvm/m3/k-${m}g.log 2>&1 | tail -n 25; done; done; } > $O/comparison-kvm.txt
CF="user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g"
{ echo "# TCG matrix, R630, restrict=on, self-test image of b6eb05d"; cat tcg/m3/queue.txt
  for c in $CF; do L=tcg/m3/$c.log; echo "$c: desktop=$(grep -ac "interactive loop started" $L) watchdog=$(grep -ac "WATCHDOG TRIPPED" $L) nmi=$(grep -ac "NMI PROBE" $L) timer-frozen=$(grep -ac "timer-probe\] FROZEN" $L) fault-lines=$(grep -aEc "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT" $L) FAILED-lines=$(grep -a "^\[[a-z0-9][a-z0-9_./-]*\]" $L | grep -ac FAILED) inversions=$(grep -ac "LOCK ORDER INVERSION" $L)"; done; } > $O/tcg-summary.txt
{ echo "# markers-compare.py: validate-2026-11d tcg-*.markers -> b6eb05d"; for c in $CF; do grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" tcg/m3/$c.log | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]" > $O/tcg-$c.markers; echo; echo "== $c"; python3 $S/markers-compare.py compare base/tcg-$c.markers $O/tcg-$c.markers | tail -n 15; done; } > $O/comparison-tcg.txt
rm -f $O/tcg-*.markers
{ echo "# gui-smoke.py, public image of b6eb05d, KVM 4 vCPU 4 GiB"; head -n 80 runs/gs-m3/report.txt; echo; echo "crash or watchdog lines: $(grep -aEc "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT|\[idt\] PAGE FAULT|WATCHDOG TRIPPED" runs/gs-m3/serial.log)"; } > $O/gui-smoke-public.txt
grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" runs/gs-m3/serial.log | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]" > $O/pub.markers
{ echo "# markers-compare.py, public image: validate-2026-11d (base/pub-final.markers) -> b6eb05d"; python3 $S/markers-compare.py compare base/pub-final.markers $O/pub.markers 2>&1 | tail -n 20; } > $O/comparison-public.txt
rm -f $O/pub.markers
ls $O
