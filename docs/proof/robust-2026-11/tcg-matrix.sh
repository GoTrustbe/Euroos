#!/usr/bin/env bash
# The four TCG selftest boots of this proof, two at a time, then the verdict
# lines of each log (.markers) and the comparison against the last proof.
# usage: tcg-matrix.sh IMG OUTDIR BASEDIR   (BASEDIR = docs/proof/net-worker-2026-11)
set -u
IMG=$1 OUT=$2 BASE=$3
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
mkdir -p "$OUT"
qa() { for r in "user-smp4-iommu 512M 4 1 user" "storage-smp4-iommu-1g 1G 4 1 storage"; do
  set -- $r; bash "$HERE/tcg-boot.sh" $1 $2 $3 $4 $5 "$IMG" "$OUT" >> "$OUT/queue-a.txt" 2>&1; done; }
qb() { for r in "user-smp1-noiommu 512M 1 0 user" "combined-smp4-iommu-1g 1G 4 1 combined"; do
  set -- $r; bash "$HERE/tcg-boot.sh" $1 $2 $3 $4 $5 "$IMG" "$OUT" >> "$OUT/queue-b.txt" 2>&1; done; }
qa & qb & wait
: > "$OUT/comparison-tcg.txt"
for n in user-smp4-iommu user-smp1-noiommu storage-smp4-iommu-1g combined-smp4-iommu-1g; do
  # Verdict lines, the lock-order total and this proof's own markers.
  grep -aE '^\[[a-z0-9][a-z0-9_./-]*\].*(✓|→ OK|FAILED|⬜)|^\[lk\] boot-total order|^\[fork\].*outlived' "$OUT/$n.log" > "$OUT/tcg-$n.markers"
  python3 "$ROOT/scripts/markers-compare.py" compare "$BASE/tcg-$n.markers" "$OUT/tcg-$n.markers" >> "$OUT/comparison-tcg.txt"
  echo >> "$OUT/comparison-tcg.txt"
done
echo "matrix done $(date +%T)" >> "$OUT/queue-a.txt"
