#!/bin/bash
# collect.sh: text results of the final validation into out/ (no images, no raw serial logs)
B=/srv/euroos/final-runs; S=$B/scripts; O=$B/out; cd $B; mkdir -p $O/screenshots
# KVM self-test boots
KEY="^\[memt\]|^\[syncsock\]|^\[many-threads\]|^\[churn-forkthreads\]|^\[fdsem\]|^\[pcmch\]|^\[hda\]|^\[churn-fork\]"
{ echo "# KVM self-test boots of the final image, R630, 2 vCPU, -nic none, intel-hda + hda-duplex (kboot-hda.sh); 1, 2, 4, 8 GiB"; cat kvm/k-*.res
  for m in 1 2 4 8; do echo; echo "== ${m} GiB"; grep -aE "$KEY" kvm/k-${m}g.log | cut -c1-330; echo "-- FAILED lines:"; grep -a "^\[" kvm/k-${m}g.log | grep -a FAILED | cut -c1-200; echo "-- crash lines: $(grep -aEc 'KERNEL PANIC|GENERAL PROTECTION|\[idt\] PAGE FAULT|DOUBLE FAULT|panicked at|WATCHDOG|NMI PROBE|timer-probe\] FROZEN' kvm/k-${m}g.log)"; done
} > $O/kvm-selftest-boots.txt
{ echo "# markers-compare.py, KVM self-test boots of the final image against the earlier proof boots (same machine and options except -audiodev)"
  for m in 1 2 4 8; do echo; echo "== ${m} GiB: chrome-mem2 f08c678 (t2) -> final"; python3 $S/markers-compare.py compare /srv/euroos/mem2-runs/kvm/t2-${m}g.log kvm/k-${m}g.log
    echo; echo "== ${m} GiB: chrome-integ af6f415 (f) -> final"; python3 $S/markers-compare.py compare /srv/euroos/integ3-runs/kvm/f-${m}g.log kvm/k-${m}g.log; done
} > $O/comparison-kvm.txt
# q35
{ echo "# CI q35 matrix, KVM, every NIC restrict=on (EK_RESTRICT=1 EK_ACCEL=kvm, 13 legs), self-test image of the merged tree"; cat q35/matrix.out | cut -c1-300
  echo; echo "# the hwprobe leg alone, repeated (one line per run)"
  for f in hw-st1 hw-st2 hw-st3 hw-us hw-chat hw-integ hw-integ2 hw-mem2 hw-audio hw-base hw-base2 hw-sttcg hw-plain1 hw-plain2 hw-rel1 hwq-final-1 hwq-final-2 hwq-final-3 hwq-integ-1 hwq-integ-2 hwq-integ-3 hwq-base-1 hwq-base-2 hwq-base-3; do echo "$f: $(grep -a '^  hwprobe' q35/$f.out | tail -n 1)"; done
} > $O/q35-matrix.txt
# gui-smoke
for r in gs-pub:public-final gs-rel:public-2026.11.13 gs-soak:public-final-monkey-soak gs-e1000e:public-final-e1000e; do n=${r%%:*}; t=${r##*:}; { echo "# gui-smoke, $t"; cat runs/$n/report.txt | head -n 120; } > $O/gui-smoke-$t.txt; done
for r in gs-pub gs-rel gs-soak gs-e1000e; do echo "== $r: $(grep -aEc 'KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT|\[idt\] PAGE FAULT|WATCHDOG TRIPPED' runs/$r/serial.log) crash or watchdog lines in $(wc -l < runs/$r/serial.log) serial lines"; done > $O/gui-smoke-serial-summary.txt
# public image against 2026.11.13
mm() { grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" "$1" | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]"; }
mm runs/gs-pub/serial.log > $O/pub-final.markers; mm runs/gs-rel/serial.log > $O/pub-2026.11.13.markers
{ echo "# markers-compare.py: public image 2026.11.13 (the download) -> public image of the merged tree, both from the gui-smoke boot"; python3 $S/markers-compare.py compare $O/pub-2026.11.13.markers $O/pub-final.markers; } > $O/comparison-public.txt
# wizard
{ cat wizard.out | cut -c1-250; } > $O/wizard-install-test.txt
# boot time
{ echo "# boot-time.py --pin 38-39 --hold 5, public image of the merged tree (final) and the public image 2026.11.13 (rel), alternating"
  echo "== while 14 other guests ran (first block)"; cat bt/kvm-ab.txt | cut -c1-150
  if [ -d bt2 ]; then echo "== second block, quiet machine"; cat bt2/kvm-ab.txt | cut -c1-150; fi
} > $O/boot-time.txt
# rename-persist
{ echo "# rename-persist.py, public image installed on a blank disk by the wizard, KVM; per run: phase 0 verdict and checks"
  for d in rp rp2 rp3 rp4 rp5 rp6 rp7 rp8 rq1 rq2 rq3 rq4 rq5 rq6 rq7 rq8 rq9; do echo "final $d: $(grep -a 'phase 0' $d.out | head -n 1 | cut -c15-80) | $(grep -a 'checks ok$' $d.out | tail -n 1 | cut -c15-40)"; done
  for d in rprel rprel2 rprel3 rq10 rq11 rq12; do echo "2026.11.13 $d: $(grep -a 'phase 0' $d.out | head -n 1 | cut -c15-80) | $(grep -a 'checks ok$' $d.out | tail -n 1 | cut -c15-40)"; done
  for d in rq13 rq14 rq15; do echo "40f853a-public $d: $(grep -a 'phase 0' $d.out | head -n 1 | cut -c15-80) | $(grep -a 'checks ok$' $d.out | tail -n 1 | cut -c15-40)"; done
  for d in ri1 ri2 ri3 ri4 ri5 ri6; do echo "integ-public $d: $(grep -a 'phase 0' $d.out 2>/dev/null | head -n 1 | cut -c15-80) | $(grep -a 'checks ok$' $d.out 2>/dev/null | tail -n 1 | cut -c15-40)"; done
  for d in rm1 rm2 rm3; do echo "mem2-public $d: $(grep -a 'phase 0' $d.out 2>/dev/null | head -n 1 | cut -c15-80) | $(grep -a 'checks ok$' $d.out 2>/dev/null | tail -n 1 | cut -c15-40)"; done
  echo; echo "== the failed checks of one complete run (rp2)"; grep -a FAILED rp2.out | cut -c1-250
  echo; echo "== phase 0 failure, serial log of the installer (rq4): the q1x3 line"; grep -a "q1x3\]" rq4/install.serial.log | cut -c1-330
  echo "== same, a passing run (rq1)"; grep -a "q1x3\]" rq1/install.serial.log | cut -c1-330
} > $O/rename-persist.txt
# short boots
{ echo "# 100 short boots of the self-test image (short-boots.sh, KVM -cpu host, 2 GiB, smp 2, no NIC), each to the [rename] verdict, six lanes"
  python3 $S/short_summary.py short-a short-b short-c short-d short-e short-f
  echo; echo "(the marker line of every boot is the [rename] verdict; FAILED lines are counted up to and including it)"; echo "== FAILED lines, any tag, all boots (count, line)"; cat short-*/failed-*.txt 2>/dev/null | cut -c1-110 | sort | uniq -c
} > $O/short-boots.txt 2>&1
# chrome
for r in starts20 starts20b starts20c; do cp runs/$r/starts.txt $O/chrome-$r.txt 2>/dev/null; done
{ echo "# chrome-smoke --only sites --rounds 3 (one session)"; grep "gui-smoke\] \(ok\|FAIL\)\|chrome-smoke\] \(PASS\|FAIL\)" runs/sites3.out | cut -c1-330; echo "-- scheduler census (last), aborts, refused forks, oom, timer:"; grep -a "^\[fds\] scheduler" runs/sites3/serial.log | tail -n 1; echo "aborts=$(grep -ac '^\[abort\]' runs/sites3/serial.log) spawn-refused=$(grep -ac spawn-refused runs/sites3/serial.log) oom=$(grep -ac '^\[oom\]' runs/sites3/serial.log) timer-frozen=$(grep -ac 'timer-probe\] FROZEN' runs/sites3/serial.log)"; } > $O/chrome-site-matrix.txt
{ echo "# chrome-smoke --only interact"; grep "gui-smoke\] \(ok\|FAIL\)\|chrome-smoke\] \(PASS\|FAIL\)" runs/interact.out | cut -c1-300; } > $O/chrome-interact.txt
{ echo "# chrome-smoke --only localvideo, intel-hda + hda-duplex, -audiodev wav"; grep "local video\|chrome-smoke\] \(PASS\|FAIL\)" runs/localvideo.out | cut -c1-330; } > $O/chrome-localvideo.txt
{ echo "# Wikimedia Commons Big Buck Bunny 480p VP9 + 6-channel Opus, scripts/wmvideo.py (video_play of chrome-mem-soak.py), 2 x 90 s"; grep "video-play\|wmvideo" runs/wm480.out | cut -c1-500; grep -a "^\[alsa\] @[0-9]* \(open\|close\|hw_params\)" runs/wm480/serial.log | cut -c1-200 | head -n 6; } > $O/chrome-wikimedia-480p.txt
cp runs/wm480/video-play-r1-02.png $O/screenshots/wm-480p-final.png 2>/dev/null
echo collected
