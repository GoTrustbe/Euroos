#!/bin/bash
# collect-c.sh: the browser, soak and boot-time results (appended to out/final)
B=/srv/euroos/val-runs; S=$B/scripts; O=$B/out/final; cd $B
CR="KERNEL PANIC|GENERAL PROTECTION|\[idt\] PAGE FAULT|DOUBLE FAULT|panicked at|WATCHDOG|NMI PROBE"
rpt() { # NAME DIR
  [ -f runs/$1.out ] || return; echo "# $2"; grep -a "gui-smoke\] \(ok\|FAIL\)\|chrome-smoke\] \(PASS\|FAIL\|CRASH\|starts\|start \)" runs/$1.out | cut -c1-330
  L=$B/runs/$1/serial.log; [ -f $L ] && echo "-- aborts=$(grep -ac '^\[abort\]' $L) spawn-refused=$(grep -ac spawn-refused $L) oom=$(grep -ac '^\[oom\]' $L) timer-frozen=$(grep -ac 'timer-probe\] FROZEN' $L) crash/watchdog=$(grep -aEc "$CR" $L)"; }
{ echo "# chrome-smoke --starts 20 --smp 2, browser image of 48dfd05 (first run), KVM, R630, tmpfs work directories"; grep -a "start [0-9]*/20\|starts:" runs/starts20.out | cut -c1-200
  echo "timer-probe FROZEN lines in the 20 starts: $(cat /dev/shm/reg/starts20/start-*/serial.log 2>/dev/null | grep -ac 'timer-probe\] FROZEN')"
  echo; echo "# second run, same image, started beside other load"; grep -a "start [0-9]*/20\|starts:" runs/starts20-v1b.out | cut -c1-200; echo "timer-probe FROZEN: $(cat /dev/shm/val/starts20-v1b/start-*/serial.log 2>/dev/null | grep -ac 'timer-probe\] FROZEN')"
  echo; echo "# 3ecde9e (release 2026.11.14 code) browser image, same host, same time as the second run"; grep -a "start [0-9]*/20\|starts:" runs/starts20-b0.out | cut -c1-200; echo "timer-probe FROZEN: $(cat /dev/shm/val/starts20-b0/start-*/serial.log 2>/dev/null | grep -ac 'timer-probe\] FROZEN')"; } > $O/chrome-starts20.txt
{ rpt sites3 "chrome-smoke --only sites --rounds 3 --smp 2, browser image of 48dfd05, one session"; echo; rpt sites-smp "informational: smpuser=on, --smp 4, same matrix"; } > $O/chrome-site-matrix.txt
rpt interact "chrome-smoke --only interact --smp 2" > $O/chrome-interact.txt
rpt localvideo "chrome-smoke --only localvideo --smp 2 (H.264/AAC fails by design: open codecs only)" > $O/chrome-localvideo.txt
{ rpt wm480 "wmvideo.py: Wikimedia Big Buck Bunny 480p VP9 + 6-channel Opus, 2 x 90 s, --smp 2, 6 GiB"; } > $O/chrome-wikimedia-480p.txt
{ echo "# keylat: chrome-smoke --only perf (30 keys), 3ecde9e (b0) against 48dfd05 (v1), four runs at a time, tmpfs work directories, smp 4 and smp 2"
  for r in 1 2 3; do echo "## round $r"; bash psum.sh kv$r-b0-4 kv$r-v1-4 kv$r-b0-2 kv$r-v1-2 2>&1 | grep "^== kv.*median" | cut -c1-120; done
  for r in 1 2 3; do [ -f runs/kq$r-b0-4.out ] || continue; echo "## quiet round $r (soak and browser session finished)"; bash psum.sh kq$r-b0-4 kq$r-v1-4 kq$r-b0-2 kq$r-v1-2 2>&1 | grep "^== kq.*median" | cut -c1-120; done; } > $O/keylat.txt
# soak
w=soakv2/w; L=$(ls $w/loadtest-serial-*.log | head -1); cp $w/loadtest-results.csv $O/r630-soak.csv
{ echo "# Soak, R630 KVM, self-test image of 48dfd05: loadtest.py --kvm --smp 4 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd ... --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 7500; pinned to CPUs 30-33, their SMT siblings (10-13) idle"
  python3 $S/soak_compare.py "48dfd05=$w/loadtest-results.csv" "d2-reg-r2=../reg-runs/soakr2/w/loadtest-results.csv" "validate-2026-11d=../final-runs/out/r630-soak-final.csv" 2>&1
  echo "serial: $(basename $L) $(stat -c %s $L) bytes"
  for k in "GLIBC-DYNAMIC-OK" "KERNEL PANIC" "GENERAL PROTECTION" "[idt] PAGE FAULT" "DOUBLE FAULT" "NMI PROBE" "WATCHDOG TRIPPED" "refused to free" "timer-probe] FROZEN" "[spawn-refused]" "[oom]" "LOCK ORDER INVERSION" "200M spins" "FROZEN"; do echo "  '$k': $(grep -acF "$k" $L)"; done
  echo "  [churn-proc] verdict lines: $(grep -ac '^\[churn-proc\]' $L), with FAILED: $(grep -a '^\[churn-proc\]' $L | grep -c FAILED)"
  echo "--- end of the harness output:"; tail -n 6 soakv2/out.txt | cut -c1-420; } > $O/r630-soak-summary.txt
# soak 2 h browser session
{ echo "# chrome-mem-soak.py --no-video --free-video-seconds 0 --hours 2 --local-video-dir (vp9.webm: VP9+Opus) --local-seconds 60, browser image of 48dfd05, 8 GiB, 2 vCPU, KVM, CPUs 0-1 (SMT siblings idle)"; grep -a "chrome-mem-soak\]\|chrome-smoke\] \(PASS\|FAIL\|CRASH\|running\)\|gui-smoke\] FAIL" runs/soak2h.out | cut -c1-260
  L=runs/soak2h/serial.log; echo "-- aborts=$(grep -ac '^\[abort\]' $L) spawn-refused=$(grep -ac spawn-refused $L) oom=$(grep -ac '^\[oom\]' $L) timer-frozen=$(grep -ac 'timer-probe\] FROZEN' $L) crash/watchdog=$(grep -aEc "$CR" $L) mm-fail=$(grep -ac '^\[mm-fail\]' $L)"; } > $O/chrome-soak2h.txt
python3 $S/mem-curve.py runs/soak2h/serial.log runs/soak2h.out > $O/chrome-soak2h-curve.txt 2>&1
# boot time
{ echo "# boot-time.py, public images, KVM, pin 38-39, alternating 48dfd05 (v1) and the 2026.11.14 download (rel); desktop = seconds from QEMU start (with the other load of the loaded block, then on a quiet machine)"
  for d in bt1 bt2; do [ -f $d/kvm-ab.txt ] || continue; echo "== $d"; cat $d/kvm-ab.txt | cut -c1-120; python3 - $d/kvm-ab.txt <<PY
import sys,statistics,re
v={}
for l in open(sys.argv[1]):
    m=re.match(r"(\w+)-\d+: .*desktop ([0-9.]+)s",l)
    if m: v.setdefault(m.group(1),[]).append(float(m.group(2)))
for k,x in v.items(): print(f"   {k}: n={len(x)} median {statistics.median(x):.2f} s min {min(x):.2f} max {max(x):.2f}")
PY
  done
  echo; echo "== kernel phases of one boot of each (the [boot-time] lines): smp-timer"
  for d in bt1 bt2; do for n in v1-4 rel-4; do [ -f $d/$n.tlog ] && echo "$d/$n: $(grep -a 'phase smp-timer' $d/$n.tlog | sed 's/^ *//' | cut -c1-90)"; done; done; } > $O/boot-time.txt
cat $O/boot-time.txt | head -40; ls $O
# BUG-271 bisect and fuzz A/B
{ echo "# [smp] lines of self-test images at -smp 2 (kj/: boots stopped at the J1 line); images: y1 eedd5bf, y2 875eb9e (S3), y3 0018719 (S5), y4 80c028d (S6)"
  for y in y1 y2 y3 y4; do echo "== $y"; grep -a "per-CPU scheduler\|J1: demo" kj/$y.log | cut -c1-170; done
  echo; echo "# the same lines from the full serial logs: validate-2026-11d, d2 9cbebe8, 48dfd05 (KVM 4 GiB boots, 2 vCPU)"
  for f in ../final-runs/kvm/k-4g.log ../d2-runs/kvm/b8/k-4g.log kvm/v1/k-4g.log; do echo "== $f"; grep -a "per-CPU scheduler\|J1: demo\|^\[kwork\]" $f | cut -c1-200; done
  echo; echo "# public images: [smp] AP line: download 2026.11.14 and 48dfd05"; grep -a "AP(s) idle in hlt" runs/gs-rel/serial.log runs/gs-pub/serial.log | cut -c1-170; } > $O/ap-bisect.txt
{ echo "# [fuzz] FAILED / boots, short-boots.sh with the marker [fuzz] Linux-ABI (2 vCPU lanes, KVM, -nic none); images: v1 48dfd05, d11 validate-2026-11d, b8 d2 9cbebe8, x1 c70dddf, x2 0cf75e6, x3 f42be2a"
  echo "# fz/ : lanes on SMT-sibling pairs next to other guests (fv1* beside fd11*, fx1 beside the power legs); fz2/: a busy loop on every sibling; fz3/: repeat of fz/"
  for d in fz fz2 fz3; do for l in $(ls $d | grep -v noise); do [ -f $d/$l/summary.txt ] && echo "$d/$l: boots $(grep -c ^boot= $d/$l/summary.txt), [fuzz] FAILED $(grep -al "kernel-survived=false" $d/$l/boot-*.log 2>/dev/null | wc -l)"; done; done
  echo; echo "# a failing boot, the end of the fuzz run:"; f=$(grep -al "kernel-survived=false" fz/fv1a/boot-*.log | head -1); grep -a "TIMEOUT waiting\|gfuzz\|\[fuzz\]\|sock_connect fd. -> 0.0.0.0" $f | cut -c1-200 | head -8
  echo "# the same stage ticks in passing KVM boots: validate-2026-11d $(grep -a "sock_connect fd. -> 0.0.0.0" ../final-runs/kvm/k-4g.log | cut -c1-30 | tr "\n" " ") | 48dfd05 $(grep -a "sock_connect fd. -> 0.0.0.0" kvm/v1/k-4g.log | cut -c1-30 | tr "\n" " ")"; } > $O/fuzz-ab.txt
cat $O/fuzz-ab.txt | head -12
