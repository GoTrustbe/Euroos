#!/bin/bash
# collect.sh: text results of the sched-d1 runs into out/ (no images, no raw serial logs)
B=/srv/euroos/sched-runs; S=$B/scripts; O=$B/out; cd $B; rm -rf $O; mkdir -p $O/drivers
cp sched.py cm.sh kboot-hda.sh tcgboot.sh tcgmatrix.sh tcgq1.sh phaseB.sh collect.sh $O/drivers/
diff -u /srv/euroos/fix190-runs/scripts/chrome-smoke.py scripts/chrome-smoke.py > $O/drivers/chrome-smoke-sched-stat.diff
# step tests
{ echo "# targeted step tests: sched.py IMAGE (KVM, -nic none), serial console commands; image tags: s0 = ab8e32c, s1 = cdf5aff, s1t = 2ef9d7b (S1 + the new [wakelat]), s2 = 80c0d69, s8 = 8fe1de9; pub = public image, st = self-test image"
  for r in s0a s0b s1p4 s1p1 s0st s0st1 s1st1 s1st4 s1st-who w1t w2 s2if s8a s2b1; do [ -f runs/$r.out ] || continue; echo; echo "== $r: $(grep -a -m1 "" runs/$r/qemu.out 2>/dev/null)"; head -c 0 /dev/null
    grep -a "^\[sched.py\]\|\[fair\|\[wakelat\]\|test: \|\[sched-stat\]\|\[t0-yield\]\|CRASH\|PANIC" runs/$r.out | cut -c1-330; done; } > $O/step-tests.txt
{ echo "# [if0-window] lines (BUG-182 attribution)"; for r in s0b s0st s2if s8a; do echo "== $r"; grep -a "if0-window\] [0-9]" runs/$r/serial.log | cut -c1-260 | head -20; done; } > $O/if0-window.txt
{ echo "# keylat: chrome-smoke --only perf, browser image, KVM smp 4 (sched-stat and keylat over the same 30 keys)"
  for r in perf-s0 perf-s0b perf-s1 perf-s2 perf-s2b; do [ -f runs/$r.out ] || continue; echo; echo "== $r"; grep -a "key to screen" runs/$r.out | cut -c1-230; grep -a "\[scon\] \[sched-stat\]\|\[scon\] \[keylat\]\|\[scon\] \[t0-yield\]" runs/$r/serial.log | grep -v "window started\|no key measured\|waited.*<0.1 0 <1 0 <5 0" | cut -c1-260; done; } > $O/keylat.txt
# regression
cat cargo-test.txt > /dev/null 2>&1
{ echo "# CI q35 matrix, KVM, every NIC restrict=on (EK_RESTRICT=1 EK_ACCEL=kvm), self-test image of 8fe1de9 (all legs of the script)"; cut -c1-300 q35m/matrix.out; } > $O/q35-matrix.txt
KEY="^\[memt\]|^\[syncsock\]|^\[many-threads\]|^\[churn-forkthreads\]|^\[fdsem\]|^\[pcmch\]|^\[hda\]|^\[churn-fork\]|^\[keyfocus\]|^\[fuzz\]|^\[lk\] boot-total"
{ echo "# KVM self-test boots of 8fe1de9, R630, 2 vCPU, -nic none, intel-hda (kboot-hda.sh); 1, 2, 4, 8 GiB"; cat kvm/*.res
  for m in 1 2 4 8; do echo; echo "== ${m} GiB"; grep -aE "$KEY" kvm/k-${m}g.log | cut -c1-260; echo "-- FAILED lines:"; grep -a "^\[" kvm/k-${m}g.log | grep -a FAILED | cut -c1-200; echo "-- crash lines: $(grep -aEc "KERNEL PANIC|GENERAL PROTECTION|\[idt\] PAGE FAULT|DOUBLE FAULT|panicked at|WATCHDOG|NMI PROBE|timer-probe\] FROZEN" kvm/k-${m}g.log) lock inversions: $(grep -ac "LOCK ORDER INVERSION" kvm/k-${m}g.log)"; done; } > $O/kvm-selftest-boots.txt
{ echo "# markers-compare.py, KVM self-test boots: validate-2026-11d (/srv/euroos/final-runs/kvm) -> 8fe1de9"
  for m in 1 2 4 8; do echo; echo "== ${m} GiB"; python3 $S/markers-compare.py compare ../final-runs/kvm/k-${m}g.log kvm/k-${m}g.log; done; } > $O/comparison-kvm.txt
CF="user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g"
{ echo "# TCG matrix, R630, restrict=on, self-test image of 8fe1de9"; cat tcg/f/queue.txt
  for c in $CF; do L=tcg/f/$c.log; echo "$c: desktop=$(grep -ac "interactive loop started" $L) watchdog=$(grep -ac "WATCHDOG TRIPPED" $L) nmi=$(grep -ac "NMI PROBE" $L) timer-frozen=$(grep -ac "timer-probe\] FROZEN" $L) fault-lines=$(grep -aEc "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT" $L) FAILED-lines=$(grep -a "^\[[a-z0-9][a-z0-9_./-]*\]" $L | grep -ac FAILED) inversions=$(grep -ac "LOCK ORDER INVERSION" $L) | keyfocus $(grep -a "^\[keyfocus\]" $L | grep -o "OK ✓\|FAILED ✗")"; done; } > $O/tcg-summary.txt
for c in $CF; do grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" tcg/f/$c.log | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]" > $O/tcg-$c.markers; done
{ echo "# markers-compare.py: validate-2026-11d tcg-*.markers -> 8fe1de9"; for c in $CF; do echo; echo "== $c"; python3 $S/markers-compare.py compare /srv/euroos/sched-runs/base/tcg-$c.markers $O/tcg-$c.markers; done; } > $O/comparison-tcg.txt
{ echo "# gui-smoke.py, public image of 8fe1de9, KVM 4 vCPU 4 GiB"; head -n 120 runs/gs-pub/report.txt; echo; echo "crash or watchdog lines: $(grep -aEc "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT|\[idt\] PAGE FAULT|WATCHDOG TRIPPED" runs/gs-pub/serial.log)"; } > $O/gui-smoke-public.txt
grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" runs/gs-pub/serial.log | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]" > $O/pub-d1.markers
python3 $S/markers-compare.py compare base/pub-final.markers $O/pub-d1.markers > $O/comparison-public.txt 2>&1
for r in sites3 interact localvideo; do { echo "# chrome-smoke --only $r, browser image of 8fe1de9, KVM"; grep "gui-smoke\] \(ok\|FAIL\)\|chrome-smoke\] \(PASS\|FAIL\)\|currentTime\|windows" runs/$r.out | cut -c1-330; echo "aborts=$(grep -ac "^\[abort\]" runs/$r/serial.log) spawn-refused=$(grep -ac spawn-refused runs/$r/serial.log) oom=$(grep -ac "^\[oom\]" runs/$r/serial.log) timer-frozen=$(grep -ac "timer-probe\] FROZEN" runs/$r/serial.log) nmi-probes=$(grep -ac "NMI PROBE (wedge" runs/$r/serial.log) lock-inversions=$(grep -ac "LOCK ORDER INVERSION" runs/$r/serial.log) if0-windows=$(grep -ac "if0-window\] [0-9]" runs/$r/serial.log)"; grep -a "if0-window\] [0-9]" runs/$r/serial.log | grep -o "cause=[a-z]*[ 0-9]*" | sort | uniq -c; } > $O/chrome-$r.txt; done
L=$(ls soak/w/loadtest-serial-*.log | head -1)
cp soak/w/loadtest-results.csv $O/r630-soak.csv
{ echo "# loadtest soak, self-test image of 8fe1de9: --kvm --smp 4 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 7500"
  python3 $S/analyze_soak.py soak/w /dev/null 2>&1
  for k in "KERNEL PANIC" "GENERAL PROTECTION" "[idt] PAGE FAULT" "DOUBLE FAULT" "NMI PROBE" "WATCHDOG TRIPPED" "timer-probe] FROZEN" "[spawn-refused]" "[oom]" "LOCK ORDER INVERSION"; do echo "  : $(grep -acF "$k" $L)"; done
  echo "  [churn-proc] verdict lines: $(grep -ac "^\[churn-proc\] [0-9]" $L), with FAILED: $(grep -a "^\[churn-proc\] [0-9]" $L | grep -c FAILED)"
  echo "  churn proc 8 rounds (ms): $(grep "churn proc 8" $O/r630-soak.csv | awk -F, "{print \$0}" | wc -l)"
  echo "== desktop-loop gaps over 10 s"; grep -a "away [0-9]\{5\} ms" $L | cut -c1-400
  echo "== [if0-window] causes"; grep -a "if0-window\] [0-9]" $L | grep -o "cause=[a-z]*[ 0-9]*" | sort | uniq -c
  echo "--- end of the harness output:"; tail -n 6 soak/out.txt | cut -c1-420
} > $O/r630-soak-summary.txt
python3 $S/soak_compare.py validate-11d=/srv/euroos/final-runs/out/r630-soak-final.csv fix190=/srv/euroos/fix190-runs/soak/w/loadtest-results.csv d1=$O/r630-soak.csv > $O/r630-soak-compare.txt 2>&1
echo collected; ls $O
