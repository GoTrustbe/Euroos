#!/bin/bash
# collect-v.sh: text results of the validation of 48dfd05 (validate-2026-11e) into out/ (no images, no raw serial logs)
B=/srv/euroos/val-runs; S=$B/scripts; O=$B/out/final; cd $B; rm -rf $O; mkdir -p $O/drivers
cp ab.sh cm.sh cms.sh kboot-hda.sh tcgmatrix.sh tcgq1.sh tcgboot.sh klatv.sh pw.sh inst.py psum.sh if0sum.py $O/drivers/ 2>/dev/null; cp pw/direct/run.sh $O/drivers/power-direct.sh
CR="KERNEL PANIC|GENERAL PROTECTION|\[idt\] PAGE FAULT|DOUBLE FAULT|panicked at|WATCHDOG|NMI PROBE|timer-probe\] FROZEN"
KEY="^\[root-heap\]|^\[memt\]|^\[cow|^\[aslr\]|^\[rename|^\[linux-rand\]|^\[nmi-bound\]|^\[fair|^\[many-threads\]|^\[churn-fork|^\[fdsem\]|^\[hda\]|^\[keyfocus\]|^\[lk\] boot-total|^\[smp\]|^\[ap-tss\]|^\[kwork\]|^\[fuzz\]"
# a. KVM boots
{ echo "# KVM self-test boots of the self-test image of 48dfd05, R630, 2 vCPU, -nic none, intel-hda + hda-duplex (kboot-hda.sh); 1, 2, 4, 8 GiB"; cat kvm/v1/*.res
  for m in 1 2 4 8; do echo; echo "== ${m} GiB"; grep -aE "$KEY" kvm/v1/k-${m}g.log | cut -c1-240; echo "-- FAILED lines:"; grep -a "^\[" kvm/v1/k-${m}g.log | grep -a FAILED | cut -c1-200; echo "-- crash lines: $(grep -aEc "$CR" kvm/v1/k-${m}g.log) lock inversions: $(grep -ac "LOCK ORDER INVERSION" kvm/v1/k-${m}g.log)"; done; } > $O/kvm-selftest-boots.txt
{ echo "# markers-compare.py, KVM self-test boots: validate-2026-11d (final-runs/kvm) and d2-9cbebe8 (d2-runs/kvm/b8) -> 48dfd05"
  for base in final-runs/kvm:validate-2026-11d d2-runs/kvm/b8:d2-9cbebe8; do d=${base%%:*}; n=${base##*:}
    for m in 1 2 4 8; do echo; echo "== $n ${m} GiB"; python3 $S/markers-compare.py compare ../$d/k-${m}g.log kvm/v1/k-${m}g.log 2>&1 | tail -n 25; done; done; } > $O/comparison-kvm.txt
# b. TCG
CF="user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g"
{ echo "# TCG matrix, R630, restrict=on, self-test image of 48dfd05"; cat tcg/v1/queue.txt
  for c in $CF; do L=tcg/v1/$c.log; echo "$c: desktop=$(grep -ac "interactive loop started" $L) watchdog=$(grep -ac "WATCHDOG TRIPPED" $L) nmi=$(grep -ac "NMI PROBE" $L) timer-frozen=$(grep -ac "timer-probe\] FROZEN" $L) fault-lines=$(grep -aEc "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT" $L) FAILED-lines=$(grep -a "^\[[a-z0-9][a-z0-9_./-]*\]" $L | grep -ac FAILED) inversions=$(grep -ac "LOCK ORDER INVERSION" $L)"; done; } > $O/tcg-summary.txt
{ echo "# markers-compare.py: validate-2026-11d tcg-*.markers -> 48dfd05"; for c in $CF; do grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" tcg/v1/$c.log | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]" > $O/tcg-$c.markers; echo; echo "== $c"; python3 $S/markers-compare.py compare base/tcg-$c.markers $O/tcg-$c.markers | tail -n 25; done; } > $O/comparison-tcg.txt
rm -f $O/tcg-*.markers
# c. q35
{ echo "# CI q35 matrix, KVM, every NIC restrict=on (EK_RESTRICT=1 EK_ACCEL=kvm), 13 legs, self-test image of 48dfd05 (tpm and tpmcrb: no swtpm on the R630, not run)"; cut -c1-300 q35/matrix.out; } > $O/q35-matrix.txt
{ echo "# power leg alone (pw.sh: run-metal-matrix.py --legs power, KVM, restrict=on), one line per boot"
  for d in pw/v1-? pw/d11-? pw/b8-? pw/x1-? pw/x2-? pw/x3-?; do echo "$(basename $d) | $(grep -a "power\] \(PASS\|FAIL\)" $d/out.txt | head -1 | tr -s ' ') | serial: power-button-pressed=$(grep -ac "power button pressed" $d/serial.log) shutting-down=$(grep -ac "shutting down system" $d/serial.log) last line: $(tail -n 1 $d/serial.log | cut -c1-120)"; done
  echo; echo "# power-direct.sh: boot 1 vCPU 512 MiB, wait for the desktop loop, 8 s, QMP system_powerdown, 40 s for QEMU to exit"
  for t in v1 d11; do echo "== $t ($([ $t = v1 ] && echo 48dfd05 self-test || echo validate-2026-11d self-test))"; cat pw/direct/$t.out; done; } > $O/power-leg.txt
# d. gui-smoke
for g in pub:v1-pub:"public image of 48dfd05 (no options)" soak:v1-pub:"public image of 48dfd05, --soak 35 --seed 20261114 (monkey)" input:v1-pub:"public image of 48dfd05, --input virtio" e1000e:v1-pub:"public image of 48dfd05, --net e1000e (2 vCPU)" rel:pubrel:"public image 2026.11.14 (download), for comparison" smp:v1-pubsmp:"public image of 48dfd05 with smpuser=on (informational, smp 4)"; do
  IFS=: read n im desc <<< "$g"; [ -f runs/gs-$n/report.txt ] || continue
  { echo "# gui-smoke.py, $desc, KVM 4 GiB"; head -n 120 runs/gs-$n/report.txt; echo; echo "crash or watchdog lines: $(grep -aEc "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT|\[idt\] PAGE FAULT|WATCHDOG TRIPPED" runs/gs-$n/serial.log)"; } > $O/gui-smoke-$n.txt
  grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" runs/gs-$n/serial.log | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]" > /tmp/gs-$n.markers
done
{ echo "# markers-compare.py, public image: validate-2026-11d (base/pub-final.markers) -> 48dfd05"; python3 $S/markers-compare.py compare base/pub-final.markers /tmp/gs-pub.markers 2>&1 | tail -n 25
  echo; echo "# markers-compare.py, public image: 2026.11.14 download -> 48dfd05 (both booted here, gui-smoke, KVM 4 vCPU 4 GiB)"; python3 $S/markers-compare.py compare /tmp/gs-rel.markers /tmp/gs-pub.markers 2>&1 | tail -n 25
  echo; echo "# [smp] lines, public image: download vs 48dfd05"; grep -a "^\[smp\] .*AP(s) idle" runs/gs-rel/serial.log runs/gs-pub/serial.log | cut -c1-200; } > $O/comparison-public.txt
cp /tmp/gs-pub.markers $O/pub.markers
# e. install etc.
cp wizard.out $O/wizard-install-test.txt
{ for m in plain inject ro0; do echo "== MODE=$m"; grep -a "ro-install\] \(PASS\|FAIL\)" roi-$m.out | cut -c1-200; done; } > $O/ro-install-test.txt
{ echo "# inst.py: phase 0 of rename-persist.py alone (public image installs itself on a blank 4 GiB virtio disk, KVM 2 vCPU, 2 GiB), ten at once on eight host CPUs (20-27)"
  for g in v1 rel; do echo "== $([ $g = v1 ] && echo '48dfd05 public image' || echo '2026.11.14 download')"; for f in inst/$g-*.out; do cut -c1-62 $f; done | sort | uniq -c; done; } > $O/installs-side-by-side.txt
cp rp.out $O/rename-persist.txt
# f. short boots
{ echo "# short-boots.sh: boots of the self-test image of 48dfd05 until [rename] rename over (KVM, 2 vCPU, -nic none), lanes of two CPUs"
  echo "boots: $(cat short-?/summary.txt | grep -c ^boot=)  with a FAILED line: $(cat short-?/summary.txt | grep ^boot= | grep -vc "failed-lines=0 ")  crash lines in any: $(cat short-?/summary.txt | grep ^boot= | grep -vc crash-lines=0)"
  echo "seeded: $(cat short-?/summary.txt | grep -c 'seeded=1 not-seeded=0')  [3d8]/[3d7]/[3d3]/[linux-rand] all OK: $(cat short-?/summary.txt | grep -c '3d8 ok=1 failed=0 | 3d7 ok=1 failed=0 | 3d3 ok=1 failed=0 | entropy lines=[0-9]* seeded=1 not-seeded=0 | linux-rand ok=1 failed=0')"
  echo "distinct values of each random number kind:"; cat short-?/lines-*.txt | grep -a "^\[3d8\]\|^\[3d7\]\|^\[3d3\]" | sed 's/[0-9a-f]\{16,\}/H/g' | head -0
  echo "FAILED lines in the boots:"; for f in short-?/boot-*.log; do grep -a "^\[[a-z0-9_./-]*\]" $f | grep -a FAILED | sed "s#^#$f: #" | cut -c1-170; done; } > $O/short-boots.txt
ls $O
