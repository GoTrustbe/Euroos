#!/bin/bash
# klat.sh ROUNDS: keylat (chrome-smoke --only perf; screendump AND fbread in each run), public 2026.11.16 browser image (b0) vs merge browser image (m),
# four guests at a time, smp4 on 0-3/4-7 and smp2 on 8-9/10-11, sides swap CPU sets every round, siblings 20-31 idle, tmpfs work dirs, harness of the merge (scripts/)
B=/srv/euroos/v11f-runs; cd $B
for r in $(seq ${2:-1} $1); do
  if [ $((r % 2)) = 1 ]; then A4=0-3; B4=4-7; A2=8-9; B2=10-11; else A4=4-7; B4=0-3; A2=10-11; B2=8-9; fi
  ./cms.sh kl$r-b0-4 img/b0-br.img $A4 --only perf --smp 4; ./cms.sh kl$r-m-4 img/m-br.img $B4 --only perf --smp 4
  ./cms.sh kl$r-b0-2 img/b0-br.img $A2 --only perf --smp 2; ./cms.sh kl$r-m-2 img/m-br.img $B2 --only perf --smp 2
  sleep 30
  until [ $(grep -al "steps ok, report" runs/kl$r-*.out 2>/dev/null | wc -l) -ge 4 ]; do sleep 15; done
  sleep 10
done
echo done > runs/klat.done
