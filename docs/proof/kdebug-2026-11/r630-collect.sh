#!/usr/bin/env bash
# Gather what the proof keeps from the R630 runs into /srv/euroos/kdebug-runs/out
B=/srv/euroos/kdebug-runs
cd $B
rm -rf out; mkdir -p out

boots() { # prefix okpattern title
  local pre=$1 okpat=$2
  for t in $pre-user:"user driver (default), smp 4" $pre-user-iommu:"user driver + intel-iommu, smp 4" $pre-smp1:"user driver, smp 1"; do
    d=${t%%:*}; [ -f $d/summary.txt ] || continue
    echo "# $d (${t#*:}; --scenario smoke, 4 GiB)"
    sort -t= -k2,2n -k3,3n $d/summary.txt
    echo "# $d: $(grep -c "$okpat" $d/summary.txt) of $(wc -l < $d/summary.txt) boots ok, to the desktop and through the smoke commands, the new self-tests OK"
    echo
  done
  echo "# FAILED verdict lines (count = boots that have the line):"
  cat $pre-*/s*-r*/failed-lines.txt 2>/dev/null | sed -E 's/^(\[[a-z0-9_./-]+\]).*/\1/' | sort | uniq -c
  echo
  echo "# [lk] boot-total order lines (count = boots):"
  cat $pre-*/s*-r*/keylines.txt 2>/dev/null | grep -a '^\[lk\] boot-total order' | tr -d '\r' | sort | uniq -c
  echo
  echo "# [ag-lock] (count = boots; the frame time replaced by N):"
  cat $pre-*/s*-r*/keylines.txt 2>/dev/null | grep -a '^\[ag-lock\]' | tr -d '\r' | sed -E 's/frame [0-9]+ ms/frame N ms/' | cut -c1-190 | sort | uniq -c
  echo
  echo "# [ag-lock] frame time, ms: min / median / max"
  cat $pre-*/s*-r*/keylines.txt 2>/dev/null | grep -a -o 'frame [0-9]* ms' | awk '{print $2}' | sort -n | awk '{a[NR]=$1} END {if (NR) print a[1], "/", a[int((NR+1)/2)], "/", a[NR], "(" NR " boots)"}'
  echo "# [boot-time] phase lockscreen, ms: min / median / max"
  cat $pre-*/s*-r*/keylines.txt 2>/dev/null | grep -a 'phase lockscreen' | sed -E 's/.*lockscreen: ([0-9]+)\.[0-9]+ ms.*/\1/' | sort -n | awk '{a[NR]=$1} END {if (NR) print a[1], "/", a[int((NR+1)/2)], "/", a[NR], "(" NR " boots)"}'
}

soak() { # dir prefix
  local d=$1 p=$2
  [ -f $d/w/loadtest-results-ltstat.csv ] || return
  python3 scripts/analyze_soak.py $d/w $d/out.txt > out/$p-summary.txt 2>&1
  cp $d/out.txt out/$p.txt
  cp $d/w/loadtest-results.csv out/$p.csv
  cp $d/w/loadtest-results-ltstat.csv out/$p-ltstat.csv
  L=$(ls $d/w/loadtest-serial-*.log | head -1)
  sed -n '1,/interactive loop started/p' "$L" | grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]" > out/$p-boot.markers
  {
    echo "--- serial log of the soak guest, counts over the whole run:"
    for q in "KERNEL PANIC" "GENERAL PROTECTION" "\[idt\] PAGE FAULT" "DOUBLE FAULT" "NMI PROBE" "\[watchdog\]" "refused to free" "no fd space free" "out of memory: page tables" "out of memory: vDSO" "not a loadable ELF"; do
      echo "  $q: $(grep -a -c "$q" "$L")"
    done
    echo "  [churn-proc] verdict lines: $(grep -a -c '^\[churn-proc\] .*→' "$L"), of them FAILED: $(grep -a '^\[churn-proc\] .*→' "$L" | grep -c FAILED)"
    echo "  the three new self-tests in the boot of the guest:"
    grep -a "^\[bug063\]\|^\[fdspace\]\|^\[bug053\]" "$L" | tr -d '\r' | cut -c1-330 | sed 's/^/    /'
    echo "  httpd clients (end of the harness output):"
    grep -a -i "hammer\|answered\|p50\|p99" $d/out.txt | tail -6 | sed 's/^/    /'
  } >> out/$p-summary.txt
}

{ echo "# Round 2: the final image (sha256 232a6c18...)"; echo; boots boots2 " ok fdspace-ok=1 bug053-ok=1 bug063-ok=1"; } > out/r630-boots-summary.txt
{ echo "# Round 1: the image of the build commit, before the BUG-064 self-test fix (sha256 f02e7660...); stopped at 78 boots"; echo; boots boots " ok fdspace-ok=1 bug053-ok=1"; } > out/r630-boots-first-round-summary.txt
soak soak2 r630-soak
soak soak r630-soak-first-round
[ -f bt3/kvm-ab.txt ] && cp bt3/kvm-ab.txt out/kvm-5x.txt
[ -f bt2/kvm-ab.txt ] && cp bt2/kvm-ab.txt out/kvm-5x-first-round.txt
cp r630-start.sh out/r630-start-first-round.sh 2>/dev/null
cp r630-start2.sh out/r630-start.sh 2>/dev/null
cp bt-ab3.sh out/r630-boot-time-ab.sh 2>/dev/null
ls -la out | cut -c25-

# BUG-065: base and final image side by side
{
  echo "# BUG-065: [3d8] EuroEntropy on the R630 (KVM). Base image 7858b72 (r630-base-boots.sh) and the final image"
  echo "# of the branch (r630-final-boots-a.sh, -b.sh), --scenario smoke, 4 GiB, booted at the same time on the same"
  echo "# CPU sets, 2026-11-12."
  for t in boots0:"base 7858b72" boots3:"final image of the branch"; do
    pre=${t%%:*}
    n=$(cat $pre-*/summary.txt | wc -l); ok=$(cat $pre-*/summary.txt | grep -c " ok ")
    f8=$(grep -l '^\[3d8\].*FAILED' $pre-*/s*-r*/3d-lines.txt 2>/dev/null | wc -l)
    f7=$(grep -l '^\[3d7\].*FAILED' $pre-*/s*-r*/3d-lines.txt 2>/dev/null | wc -l)
    f3=$(grep -l '^\[3d3\].*FAILED' $pre-*/s*-r*/3d-lines.txt 2>/dev/null | wc -l)
    echo
    echo "== ${t#*:}: $n boots, $ok to the desktop and clean through the smoke commands; [3d8] FAILED in $f8, [3d7] in $f7, [3d3] in $f3"
    echo "   jitter-alone estimate of every boot, bits, sorted:"
    grep -h '^\[3d8\]' $pre-*/s*-r*/3d-lines.txt | grep -o 'jitter-alone≈[0-9]*bits' | sed 's/[^0-9]//g' | sort -n | tr '\n' ' ' | fold -s -w 110 | sed 's/^/   /'
    echo
    grep -h '^\[3d[378]\].*FAILED' $pre-*/s*-r*/3d-lines.txt | cut -c1-330 | sort | uniq -c | sed 's/^/   /'
  done
  echo
  echo "# The boot loops of the two proof rounds (only the first 160 characters of a FAILED line were kept there):"
  echo "#   final image, 100 boots: [3d8] FAILED in $(grep -l '^\[3d8\]' boots2-*/s*-r*/failed-lines.txt | wc -l), [3d7] in $(grep -l '^\[3d7\]' boots2-*/s*-r*/failed-lines.txt | wc -l), [3d3] in $(grep -l '^\[3d3\]' boots2-*/s*-r*/failed-lines.txt | wc -l); jitter estimates of the failing boots: $(grep -h '^\[3d8\]' boots2-*/s*-r*/failed-lines.txt | grep -o 'jitter-alone≈[0-9]*bits' | sed 's/[^0-9]//g' | sort -n | tr '\n' ' ')"
  echo "#   first-round image, 78 boots: [3d8] FAILED in $(grep -l '^\[3d8\]' boots-*/s*-r*/failed-lines.txt | wc -l), [3d7] in $(grep -l '^\[3d7\]' boots-*/s*-r*/failed-lines.txt | wc -l), [3d3] in $(grep -l '^\[3d3\]' boots-*/s*-r*/failed-lines.txt | wc -l); jitter estimates of the failing boots: $(grep -h '^\[3d8\]' boots-*/s*-r*/failed-lines.txt | grep -o 'jitter-alone≈[0-9]*bits' | sed 's/[^0-9]//g' | sort -n | tr '\n' ' ')"
} > out/r630-bug065-base-vs-final.txt
cp r630-base-boots.sh r630-final-boots-a.sh r630-final-boots-b.sh out/ 2>/dev/null
ls -la out | cut -c25-
