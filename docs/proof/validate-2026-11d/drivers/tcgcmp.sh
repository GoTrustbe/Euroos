cd /srv/euroos/final-runs; mkdir -p out
CF="user-smp4-iommu user-smp4-noiommu kernel-smp4 storage-smp4-iommu-1g combined-smp4-iommu-1g user-smp1-iommu user-smp1-noiommu kernel-smp1 storage-smp1-iommu-1g combined-smp1-iommu-1g"
{ echo "# TCG matrix, R630, restrict=on, final image (self-test image of the merged tree)"; cat tcg/f/queue.txt
  for c in $CF; do L=tcg/f/$c.log; echo "$c: desktop=$(grep -ac "interactive loop started" $L) watchdog=$(grep -ac "WATCHDOG TRIPPED" $L) nmi=$(grep -ac "NMI PROBE" $L) timer-frozen=$(grep -ac "timer-probe\] FROZEN" $L) fault-lines=$(grep -aEc "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT" $L) FAILED-lines=$(grep -a "^\[[a-z0-9][a-z0-9_./-]*\]" $L | grep -ac FAILED) | $(grep -a "^\[memt\] memory self-test" $L | cut -c1-80) | pcmch: $(grep -a "^\[pcmch\]" $L | grep -o "→ OK\|FAILED\|not run" | head -1) | longest desk gap $(grep -ao "away [0-9]* ms" $L | sed "s/away //;s/ ms//" | sort -n | tail -1) ms"; done
} > out/tcg-summary.txt
for c in $CF; do bash /dev/stdin $PWD/tcg/f/$c.log > out/tcg-$c.markers <<'M'
grep -a -E "^\[[a-z0-9][a-z0-9_./-]*\]" "$1" | grep -a -E "✓|FAILED|⬜|→ OK|-> OK|^\[lk\] boot-total|^\[fork\]|^\[boot-time\]"
M
done
for b in chrome-integ chrome-mem2 validate-2026-11c; do { echo "# markers-compare.py: base/$b/tcg-*.markers (proof of that branch) against the final image (R630, TCG, restrict=on)"; for c in $CF; do echo; echo "== $c"; python3 scripts/markers-compare.py compare base/$b/tcg-$c.markers out/tcg-$c.markers; done; } > out/comparison-tcg-$b.txt; done
cat out/tcg-summary.txt
grep -c "WORSE: 0" out/comparison-tcg-*.txt
grep -h -A8 "WORSE: [1-9]" out/comparison-tcg-*.txt | head -40
