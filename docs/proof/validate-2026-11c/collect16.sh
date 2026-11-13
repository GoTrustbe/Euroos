#!/bin/bash
# Assemble docs/proof/validate-2026-11c from the runs on .16 (the R630 files are
# unpacked into $R/r630 before this runs). Round 1 (7be30cb) is in $R/round1, the
# round on the last code commit (f98da76) in $R.
set -u
R=/opt/euroos/validate-runs; W=/opt/euroos/wt-chrome
D=$W/docs/proof/validate-2026-11c
mkdir -p $D/round1 $D/screenshots
one() { # run directory -> proof directory
  local r=$1 d=$2
  cp $r/proof/tcg-*.markers $r/proof/comparison-tcg.txt $r/proof/tcg-summary.txt $r/proof/tcg-newtests.txt $d/
  cp $r/proof/mr-*.markers $r/proof/mr-*-host-check.txt $r/proof/comparison-mr.txt $d/ 2>/dev/null
  cp $r/proof/q35-matrix.txt $r/proof/repro-build.txt $d/
  {
    echo "# [lk] boot-total order line of every matrix boot"
    for f in $r/tcg/out/*.log; do echo "$(basename $f .log): $(grep -a '^\[lk\] boot-total order' $f | tr -d '\r' | tail -1)"; done
    echo "# problems noted by the matrix (no desktop, NMI sent): $(cat $r/tcg/out/problems.txt 2>/dev/null | wc -l)"
  } > $d/lock-tcg-lines.txt
  {
    echo "# cargo test (default members), $(cat $r/hosttests.done)"
    echo "suites with a result line: $(grep -c '^test result:' $r/hosttests.log)"
    grep "^test result" $r/hosttests.log | awk '{p+=$4; f+=$6; i+=$8} END {print p" passed, "f" failed, "i" ignored"}'
    grep "^test result" $r/hosttests.log | grep -v "ok\." | head
  } > $d/hosttests.txt
  cp $r/gs-tcg-a/report.txt $d/gui-smoke-tcg.txt
  cp $r/gs-tcg-b/report.txt $d/gui-smoke-tcg-e1000e.txt
  cp $r/wizard.out $d/wizard-install-test.txt
  {
    echo "# Public image on the open network, .16, TCG. boot-time.py --net open, commands on the serial console."
    for n in net-open net-open-e1000e; do
      echo "== $n ($(tail -1 $r/bt-$( [ $n = net-open ] && echo net || echo net-e1000e ).out))"
      grep -a "\[entropy\]\|\[scon\]\|\[euroupdate\]" $r/bt/$n.tlog | tr -d '\r' | cut -c11-240
      echo "self-test lines in this log: $(grep -acE '\[(rename|linux-rand|3d8|3d7|3d3|bug063|fdspace|bug053)\]' $r/bt/$n.tlog)"
    done
  } > $d/network-open.txt
}
one $R/round1 $D/round1
one $R $D
{
  echo "# round 1: 7be30cb"; (cd $R/build/r1 && sha256sum st.img pub.img)
  echo "# last code commit: f98da76"; (cd $R/build && sha256sum st.img pub.img)
  echo "# base for the before-and-after runs: public image of c6f033f plus the test commit (the probe in /bin)"; (cd $R/build && sha256sum pubbase.img)
  echo "# as they arrived on the R630"; cat $R/r630/images-r630.sha256
} > $D/images.sha256
cp -r $R/r630/screenshots/. $D/screenshots/
cp $R/r630/*.txt $R/r630/*.csv $R/r630/*.markers $D/ 2>/dev/null
{
  for t in 7be30cb f98da76; do
    echo "== boot of the soak guest ($t) against integ-2026-11b/r630-soak-boot.markers"
    python3 $W/scripts/markers-compare.py compare $W/docs/proof/integ-2026-11b/r630-soak-boot.markers $R/r630/r630-soak-$t-boot.markers 2>&1
    echo "== boot of the soak guest ($t) against linux-rand-2026-11/r630-soak-boot.markers"
    python3 $W/scripts/markers-compare.py compare $W/docs/proof/linux-rand-2026-11/r630-soak-boot.markers $R/r630/r630-soak-$t-boot.markers 2>&1
  done
} > $D/comparison-r630-soak-boot.txt
cp $R/run-16-selftest.sh $R/run-16-public.sh $R/build.sh $R/base-build.sh $R/base-live.sh $R/base-st.sh $R/live-build.sh $R/collect16.sh $D/
cp $R/r630/r630-start.sh $R/r630/r630-final.sh $R/r630/gtk-live.py $R/r630/gtk-loop.sh $R/r630/gtk-diag.sh $R/r630/drive.py $D/
cp $R/collect-r630.sh $D/ 2>/dev/null
ls $D | wc -l; du -sh $D
