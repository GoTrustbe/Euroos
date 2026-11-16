#!/bin/bash
# klat.sh: keylat pairs, final (f) against e4b8ba4 (b0), browser images, tmpfs work dirs, siblings idle (CPUs 4-11, siblings 24-31 unused). Team 11f-A.
B=/srv/euroos/11fa-runs; cd $B
w() { sleep 30; until [ $(grep -al "chrome-smoke\] \(PASS\|FAIL\|CRASH\)" $(for n in "$@"; do echo runs/$n.out; done) 2>/dev/null | wc -l) -ge $# ]; do sleep 15; done; }
for r in 1 2 3; do
  if [ $((r % 2)) = 1 ]; then F=4-7; O=8-11; else F=8-11; O=4-7; fi
  SCRIPTS=s2/scripts ./cms.sh k$r-f-4 img/f-br.img $F --only perf --smp 4; SCRIPTS=s2/scripts ./cms.sh k$r-b0-4 img/b0-br.img $O --only perf --smp 4
  w k$r-f-4 k$r-b0-4
done
for r in 1 2; do
  SCRIPTS=s2/scripts ./cms.sh k$r-f-2a img/f-br.img 4-5 --only perf --smp 2; SCRIPTS=s2/scripts ./cms.sh k$r-b0-2a img/b0-br.img 8-9 --only perf --smp 2
  SCRIPTS=s2/scripts ./cms.sh k$r-b0-2b img/b0-br.img 6-7 --only perf --smp 2; SCRIPTS=s2/scripts ./cms.sh k$r-f-2b img/f-br.img 10-11 --only perf --smp 2
  w k$r-f-2a k$r-b0-2a k$r-b0-2b k$r-f-2b
done
echo done > runs/klat.done
