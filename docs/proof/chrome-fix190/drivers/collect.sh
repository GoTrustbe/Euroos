#!/bin/bash
# collect.sh: text results of the fix190 runs into out/ (no images, no raw serial logs)
B=/srv/euroos/fix190-runs; S=$B/scripts; O=$B/out; cd $B; rm -rf $O; mkdir -p $O/drivers
cp hw.sh inst.py kboot-hda.sh tcgmatrix.sh tcgq1.sh tcgboot.sh cm.sh collect.sh $O/drivers/
# BUG-190
{ echo "# hwprobe leg alone (run-metal-matrix.py --legs hwprobe, KVM, EK_RESTRICT=1, 512 MiB, 1 vCPU), one line per boot"
  echo "# columns: run | verdict | [sc] lines (gsdl ate the staged demo input) | focus of the first character (diagnostic build) or [keyfocus] | staged input dropped (fix)"
  for d in q35/*/; do n=$(basename $d); v=$(grep -h -E "ALL LEGS PASS|hwprobe +FAIL" $d/out.txt | tail -1 | tr -s " ")
    f=$(grep -a -m1 "char .h. in the desktop loop" $d/serial.log | grep -o 'focus Some("[^"]*")')
    k=$(grep -a -o "\[keyfocus\].*" $d/serial.log | grep -o "OK ✓\|FAILED ✗")
    st=$(grep -a -o "staged demo input no client read: [0-9]* scancode(s), left click [a-z]*" $d/serial.log)
    echo "$n | $v | sc=$(grep -a -c '^\[sc\]' $d/serial.log) | ${f:-}${k:+keyfocus $k}${st:+ | $st}"; done
  echo; echo "# images: st-ac36955 = self-test image of ac36955 (sha256 0ebfe1b859e41e09, the validator's), st-diag1 = ac36955 + [kroute] diagnostics,"
  echo "#         st-test = 946989b + e909c2d (test and logging, no fix), st-fix1 = f178250 (+ logging), st-final = a38230d"
  echo "# base*: st-ac36955  diag1*: st-diag1  test*: st-test  fix1*: st-fix1  hwf*: st-final"
} > $O/hwprobe-runs.txt
{ echo "# [kroute] diagnostic build: the first lines of a failing (diag1-0-1) and a passing (diag1-2-1) boot"; for d in diag1-0-1 diag1-2-1; do echo "== $d"; grep -a "\[kroute\]" q35/$d/serial.log | head -6 | cut -c1-200; done; } > $O/kroute-diagnostic.txt
{ echo "# CI q35 matrix, KVM, every NIC restrict=on (EK_RESTRICT=1 EK_ACCEL=kvm), 13 legs, self-test image of a38230d"; cat q35m/matrix.out | cut -c1-300; } > $O/q35-matrix.txt
# BUG-191
{ echo "# inst.py: phase 0 of rename-persist.py alone (public image installs itself on a blank 4 GiB virtio disk, KVM 2 vCPU, 2 GiB)"
  echo "# ten side by side on eight host CPUs; d1 = public image with the logging of e909c2d (no fix), f1 = a38230d (pub-fix1), fin = a38230d (final public image)"
  for g in d1 f1 fin; do echo "== $g"; for f in inst/$g-*.out; do cut -c1-60 $f; done | sort | uniq -c; done
  echo; echo "== d1, the logged error (all ten)"; grep -h "q1x3-diag\|blk-diag" inst/d1-*/serial.log | cut -c1-200 | sed "s/after [0-9]* ms/after N ms/" | sort | uniq -c
  echo "-- the times:"; grep -ho "TIMED OUT after [0-9]* ms" inst/d1-*/serial.log | tr "\n" " "; echo
  echo; echo "== f1 and fin: slow requests (the flush that timed out before)"; grep -h "^\[blk\] slow\|did not complete\|^\[blk\] the request" inst/f1-*/serial.log inst/fin-*/serial.log | cut -c1-120
  echo; echo "== a read-only blank target (RO=1): the final merge of validate-2026-11d (plain.img) and a38230d"
  for d in ro-final ro-fix; do echo "-- $d"; grep -a "\[wizard\]\|\[q1x3\]" inst/$d/serial.log | cut -c1-300; done
} > $O/installs-side-by-side.txt
cp wizard.out $O/wizard-install-test.txt
# KVM boots
KEY="^\[memt\]|^\[syncsock\]|^\[many-threads\]|^\[churn-forkthreads\]|^\[fdsem\]|^\[pcmch\]|^\[hda\]|^\[churn-fork\]|^\[keyfocus\]|^\[fuzz\]"
{ echo "# KVM self-test boots of a38230d, R630, 2 vCPU, -nic none, intel-hda + hda-duplex (kboot-hda.sh); 1, 2, 4, 8 GiB"
  echo "## set 2 (kvm2/k-*): beside it the self-test image of ac36955 (kvm2/b-*), at the same moment"; cat kvm2/*.res
  for m in 1 2 4 8; do echo; echo "== ${m} GiB"; grep -aE "$KEY" kvm2/k-${m}g.log | cut -c1-260; echo "-- FAILED lines:"; grep -a "^\[" kvm2/k-${m}g.log | grep -a FAILED | cut -c1-200; echo "-- crash lines: $(grep -aEc 'KERNEL PANIC|GENERAL PROTECTION|\[idt\] PAGE FAULT|DOUBLE FAULT|panicked at|WATCHDOG|NMI PROBE|timer-probe\] FROZEN' kvm2/k-${m}g.log)"; done
  echo; echo "## set 1 (kvm/k-*), started together with about 30 other guests: the [fuzz] lines"; cat kvm/*.res; grep -a "^\[fuzz\]" kvm/k-*g.log | cut -c1-200
  echo; echo "## A/B at 2 GiB on a quieter machine, between the sets"; cat kvmab/*.res; grep -a "^\[fuzz\]" kvmab/*.log | cut -c1-160
} > $O/kvm-selftest-boots.txt
{ echo "# markers-compare.py, KVM self-test boots: validate-2026-11d (/srv/euroos/final-runs/kvm, final merge 0fb0472) -> a38230d (kvm2)"
  for m in 1 2 4 8; do echo; echo "== ${m} GiB"; python3 $S/markers-compare.py compare ../final-runs/kvm/k-${m}g.log kvm2/k-${m}g.log; done
  echo; echo "# and ac36955 -> a38230d, booted side by side (kvm2/b -> kvm2/k)"
  for m in 1 2 4 8; do echo; echo "== ${m} GiB"; python3 $S/markers-compare.py compare kvm2/b-${m}g.log kvm2/k-${m}g.log; done
} > $O/comparison-kvm.txt
# TCG
CF="user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g"
{ echo "# TCG matrix, R630, restrict=on, self-test image of a38230d"; cat tcg/f/queue.txt
  for c in $CF; do L=tcg/f/$c.log; echo "$c: desktop=$(grep -ac "interactive loop started" $L) watchdog=$(grep -ac "WATCHDOG TRIPPED" $L) nmi=$(grep -ac "NMI PROBE" $L) timer-frozen=$(grep -ac "timer-probe\] FROZEN" $L) fault-lines=$(grep -aEc "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT" $L) FAILED-lines=$(grep -a "^\[[a-z0-9][a-z0-9_./-]*\]" $L | grep -ac FAILED) | keyfocus $(grep -a "^\[keyfocus\]" $L | grep -o "OK ✓\|FAILED ✗")"; done
} > $O/tcg-summary.txt
for c in $CF; do grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" tcg/f/$c.log | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]" > $O/tcg-$c.markers; done
# gui-smoke, chrome
{ echo "# gui-smoke.py, public image of a38230d (no options), KVM 4 vCPU 4 GiB"; head -n 120 runs/gs-pub/report.txt; echo; echo "crash or watchdog lines: $(grep -aEc 'KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT|\[idt\] PAGE FAULT|WATCHDOG TRIPPED' runs/gs-pub/serial.log)"; } > $O/gui-smoke-public.txt
grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" runs/gs-pub/serial.log | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]" > $O/pub-fix190.markers
{ grep -a "start [0-9]*/20\|starts:" runs/starts20.out; echo "timer-probe FROZEN lines in the 20 starts: $(cat runs/starts20/start-*/serial.log 2>/dev/null | grep -ac 'timer-probe\] FROZEN')"; } > $O/chrome-starts20.txt
for r in sites3-a sites3; do [ -f runs/$r.out ] || continue; { echo "# chrome-smoke --only sites --rounds 3 (one session), run $r"; grep "gui-smoke\] \(ok\|FAIL\)\|chrome-smoke\] \(PASS\|FAIL\)" runs/$r.out | cut -c1-330; echo "-- scheduler census (last):"; grep -a "^\[fds\] scheduler" runs/$r/serial.log | tail -n 1; echo "aborts=$(grep -ac '^\[abort\]' runs/$r/serial.log) spawn-refused=$(grep -ac spawn-refused runs/$r/serial.log) oom=$(grep -ac '^\[oom\]' runs/$r/serial.log) timer-frozen=$(grep -ac 'timer-probe\] FROZEN' runs/$r/serial.log) stack-overflow=$(grep -ac 'KERNEL STACK OVERFLOW' runs/$r/serial.log) nmi-probes=$(grep -ac 'NMI PROBE (wedge' runs/$r/serial.log)"; } > $O/chrome-site-matrix-$r.txt; done
{ echo "# sites3-a, the failed step: serial lines around the kernel stack overflow (BUG-202)"; N=$(grep -an "KERNEL STACK OVERFLOW" runs/sites3-a/serial.log | head -1 | cut -d: -f1); sed -n "$((N-42)),$((N+12))p" runs/sites3-a/serial.log | grep -av "^\[nmi-tasks\] t[0-9]" | cut -c1-220; } > $O/bug202-serial.txt
# BUG-192 soak
L=$(ls soak/w/loadtest-serial-*.log); { echo "# loadtest soak of the self-test image of a38230d, 16 GiB, 4 vCPU, e1000e restrict, hammer 4, procs 32, --soak-churn-proc 8, 3300 s"; tail -n 30 soak/out.txt | cut -c1-200; echo; echo "== desktop-loop gaps over 10 s"; grep -a "away [0-9]\{5\} ms" $L | cut -c1-1100; } > $O/soak-bug192.txt
echo collected; ls $O
