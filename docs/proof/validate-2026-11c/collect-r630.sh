#!/bin/bash
# Gather the text results and the chosen screenshots of the R630 runs into $B/out
# (no images, no raw serial logs) and print a tar.gz of it on stdout.
set -u
B=/srv/euroos/validate-runs; S=$B/scripts
O=$B/out; rm -rf $O; mkdir -p $O/screenshots
cd $B

ren() { grep -a "^\[rename\]" "$1" | tr -d '\r' | cut -c1-400; }
{ echo "# [rename] self-test, self-test image of c6f033f plus the test commit de92f08 only (R630, KVM, 2 GiB, smp 2)"; ren dev/base1/boot-1.log; } > $O/rename-selftest-c6f033f.txt
{ echo "# [rename] self-test on de92f08 (the branch with the test, before the BUG-110 fix)"; ren dev/t1/boot-1.log; } > $O/rename-selftest-de92f08.txt
{ echo "# [rename] self-test on 7be30cb (the BUG-110 fix)"; ren dev/fix1/boot-1.log; } > $O/rename-selftest-7be30cb.txt
cp rpbase.out $O/rename-persist-c6f033f.txt
cp rp1.out $O/rename-persist-7be30cb.txt
cp final/rp.out $O/rename-persist-f98da76.txt
cp final/rp/phase1-terminal.png $O/screenshots/rename-persist-terminal-f98da76.png 2>/dev/null

boots() { # dir lanes... -> text
  local d=$1; shift
  ( cd $d && python3 $S/short_summary.py "$@"
    echo; echo "== [rename] verdicts over the kept serial logs"
    echo "OK: $(cat $(for l in "$@"; do ls $l/boot-*.log 2>/dev/null; done) /dev/null | grep -a '^\[rename\] rename over' | grep -c 'OK ✓'), FAILED: $(cat $(for l in "$@"; do ls $l/boot-*.log 2>/dev/null; done) /dev/null | grep -a '^\[rename\] rename over' | grep -c 'FAILED')"
    echo "glibc runs with the lazily filled mapping and step D: $(cat $(for l in "$@"; do ls $l/boot-*.log 2>/dev/null; done) /dev/null | grep -a '^\[rename\] rename over' | grep -c 'lazily filled mapping and step D ran')"
    echo; echo "== FAILED lines, any tag, over all boots (count, line)"
    cat $(for l in "$@"; do ls $l/failed-*.txt 2>/dev/null; done) /dev/null | cut -c1-110 | sort | uniq -c
    echo; for l in "$@"; do echo "== $l"; cut -c1-330 $l/summary.txt; done )
}
{ echo "# 100 short boots of the self-test image of 7be30cb, R630, KVM, -cpu host, 2 GiB, smp 2, no NIC; each to the [rename] verdict (short-boots.sh)."; boots $B short-a short-b; } > $O/r630-short-boots-7be30cb.txt
{ echo "# 100 short boots of the self-test image of f98da76, same procedure, three lanes."; boots $B/final short-a short-b short-c; } > $O/r630-short-boots-f98da76.txt

soak() { # workdir harness-out tag commit
  local w=$1 out=$2 tag=$3
  local L=$(ls $w/loadtest-serial-*.log | head -1)
  cp $w/loadtest-results.csv $O/r630-soak-$tag.csv
  cp $w/loadtest-results-ltstat.csv $O/r630-soak-$tag-ltstat.csv 2>/dev/null
  awk '/interactive loop started/{exit} {print}' "$L" | grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]" > $O/r630-soak-$tag-boot.markers
  {
    echo "# Soak, R630 KVM, self-test image of $tag: loadtest.py --kvm --smp 4 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd ... --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 7500"
    python3 $S/analyze_soak.py $w /dev/null 2>&1
    echo "serial: $(basename $L) $(stat -c %s $L) bytes"
    for k in "GLIBC-DYNAMIC-OK" "KERNEL PANIC" "GENERAL PROTECTION" "[idt] PAGE FAULT" "DOUBLE FAULT" "NMI PROBE" "[watchdog]" "refused to free" "out of memory: page tables" "no frames for page tables" "random bytes was refused" "asked for before the boot seeded"; do
      echo "  '$k': $(grep -acF "$k" $L)"
    done
    echo "  [entropy] lines: $(grep -ac '^\[entropy\]' $L)"
    echo "  [rename] verdict: $(grep -a '^\[rename\] rename over' $L | tr -d '\r' | cut -c1-260)"
    echo "  [churn-proc] verdict lines: $(grep -ac '^\[churn-proc\]' $L), with FAILED: $(grep -a '^\[churn-proc\]' $L | grep -c FAILED)"
    echo "  FAILED lines after the desktop (count, line):"
    awk 'f{print} /interactive loop started/{f=1}' "$L" | grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" | grep -a FAILED | tr -d '\r' | cut -c1-200 | sort | uniq -c | head -8
    grep -a "^\[lk\] boot-total order" $L | tr -d '\r' | tail -1
    echo "--- end of the harness output:"
    tail -6 $out | cut -c1-420
  } > $O/r630-soak-$tag-summary.txt
}
soak soak/w soak/out.txt 7be30cb
soak final/soak/w final/soak/out.txt f98da76
python3 $S/soak_compare.py 7be30cb=$O/r630-soak-7be30cb.csv f98da76=$O/r630-soak-f98da76.csv > $O/r630-soak-compare.txt 2>&1

{
  echo "# Boot time of the public image, R630 KVM, boot-time.py --pin 38-39 --hold 5 (SATA medium, smp 2, 2 GiB, restrict=on)."
  echo "== round 1: public image of 7be30cb, 5 boots, while the soak, the short boots and the scenarios below were starting"
  cat bt/kvm.txt
  for f in bt/final-*.tlog; do echo "$(basename $f .tlog): $(grep -a '\[boot-time\] desktop after' $f | sed -E 's/.*(desktop after [0-9]+ ms).*/\1/')"; done
  echo "== round 2: base (c6f033f plus the probe in /bin) and the public image of f98da76, alternating"
  cat final/bt/kvm-ab.txt
} > $O/kvm-boot-time.txt
grep -a "\[boot-time\]" final/bt/final-1.tlog | tr -d '\r' | cut -c11-400 > $O/kvm-boot-time-lines-f98da76.txt
{ echo "== public image of 7be30cb"; cat nvme-check.txt; echo; echo "== public image of f98da76"; cat final/nvme-check.txt; } > $O/nvme-check.txt
cp gs-usb/report.txt $O/gui-soak-usb-7be30cb.txt; cp gs-virtio/report.txt $O/gui-soak-virtio-7be30cb.txt
cp final/gs-usb/report.txt $O/gui-soak-usb-f98da76.txt 2>/dev/null; cp final/gs-virtio/report.txt $O/gui-soak-virtio-f98da76.txt 2>/dev/null
for t in gs-usb gs-virtio final/gs-usb final/gs-virtio; do
  echo "== $t: $(grep -aEc 'KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT|\[idt\] PAGE FAULT|WATCHDOG TRIPPED' $t/serial.log 2>/dev/null) crash or watchdog lines in $(wc -l < $t/serial.log 2>/dev/null) serial lines"
done > $O/gui-soak-serial-summary.txt

# The X server: a third-party X app in a hosted window, and the GTK demo.
x() { # outfile serial-log label
  { echo "# $3"; cat $1 2>/dev/null; echo "--- serial lines of the app, the X server's input and the Terminal:"; grep -a "\[e2e\]\|terminated + arena\|forgotten\|\[xin\] connection\|\[xin\] state\|\[xserver\] -> input\|DestroyWindow\|\[xserver\] key sc" $2 | tr -d '\r' | sed -n '/\[e2e\] \$ app run/,$p' | cut -c1-240; }
}
x x11base.out x11base/serial.log "c6f033f (self-test image): app run hello-x11, a key, a click" > $O/x11-app-c6f033f.txt
x x11b.out x11b/serial.log "7be30cb (before BUG-113): app run hello-x11, cdiag, a key, a click" > $O/x11-app-7be30cb.txt
x final/x11.out final/x11/serial.log "f98da76: app run hello-x11 three times (a key; a click; the close button), then cdiag, fstrace, ctabs, xtrace" > $O/x11-app-f98da76.txt
cp x11base/x11-drawn.png $O/screenshots/x11-app-c6f033f.png
cp x11a/x11-drawn.png $O/screenshots/x11-app-7be30cb-before-bug113.png
cp final/x11/x1-drawn.png $O/screenshots/x11-app-f98da76-first-start.png
cp final/x11/x2-drawn.png $O/screenshots/x11-app-f98da76-second-start.png
cp gtkbase2-1/desk-a.png $O/screenshots/gtk-demo-c6f033f.png; cp gtkbase/shot-600.png $O/screenshots/gtk-demo-c6f033f-first-run.png
cp gtkd-2/desk-a.png $O/screenshots/gtk-demo-7be30cb-before-bug113.png
cp gtkf-1/desk-a.png $O/screenshots/gtk-demo-f98da76.png
{
  echo "# The GTK demo (ggtk), self-test image built with --features live-demos, R630 KVM, 4 GiB, smp 2 (gtk-live.py, gtk-loop.sh, gtk-diag.sh)."
  echo "== c6f033f, first run (gtk-live.py, 240 s of screenshots)"; grep -v "j1-kmsg" gtkbase.out | cut -c1-200
  echo "== c6f033f, more runs (gtk-diag.sh)"; cat gtkbase2-loop.txt gtkbase3-loop.txt 2>/dev/null; echo "   input queued unread lines per run: $(for d in gtkbase2-[0-9] gtkbase3-[0-9]; do grep -ac "queued unread" $d/serial.log; done | tr "
" " ")"
  echo "== 7be30cb (before BUG-113), gtk-live.py"; grep -v "j1-kmsg" gtk1.out | cut -c1-200
  echo "== 7be30cb, gtk-loop.sh (runs that ended at the loader error are counted by their error line)"; cat gtkb-loop-first.txt gtkc-loop.txt 2>/dev/null
  echo "== 7be30cb, gtk-diag.sh"; cat gtkd-loop.txt
  echo "== f98da76 (the working tree that became it), gtk-diag.sh"; cat gtkf-loop.txt
  echo "== f98da76, the run of the screenshot: lines of the app, the demo test and the X server's input state"
  grep -a "GGTK\|gtk-test\|forgotten\|\[xin\] conn\|\[xin\] state\|\[xin\] ->" gtkf-1/serial.log | tr -d '\r' | cut -c1-240 | head -24
} > $O/gtk-demo.txt
{
  echo "# The lines of the X11, Cairo, Pango and other glibc self-tests in one boot of the self-test image (KVM, 2 GiB), c6f033f plus the test commit against 7be30cb."
  echo "# Addresses, milliseconds and counters replaced by N. diff base new:"
  diff dev/x-base.txt dev/x-fix.txt
  echo "# lines compared: $(wc -l < dev/x-base.txt) and $(wc -l < dev/x-fix.txt)"
  echo "# the X clients of the suite, 7be30cb:"
  grep -a "gxwin\|GXWIN\|gcairo\|gpango\|gxlive\|GXLIVE\|gsdl\|GSDL" dev/x-fix.txt
} > $O/x-selftests-base-vs-branch.txt
cp scripts/gtk-live.py scripts/gtk-loop.sh scripts/gtk-diag.sh scripts/drive.py r630-start.sh r630-final.sh $O/
sha256sum st.img pub.img stf.img pubf.img pubbase.img stlive-base.img stlive.img stlive3.img 2>/dev/null > $O/images-r630.sha256
tar czf - -C $O .
