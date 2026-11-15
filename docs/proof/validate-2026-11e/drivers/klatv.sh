#!/bin/bash
# klatv.sh ROUNDS: keylat 3ecde9e (b0) against the release candidate (v1), smp 4 and smp 2, four runs at a time, tmpfs work dirs
B=/srv/euroos/val-runs; cd $B
for r in $(seq 1 $1); do
  ./cms.sh kv$r-b0-4 img/b0-br.img 34-37 --only perf --smp 4; ./cms.sh kv$r-v1-4 img/v1-br.img 2-5 --only perf --smp 4
  ./cms.sh kv$r-b0-2 img/b0-br.img 38-39 --only perf --smp 2; ./cms.sh kv$r-v1-2 img/v1-br.img 28-29 --only perf --smp 2
  sleep 30
  until [ $(grep -al "smoke\] \(PASS\|FAIL\|CRASH\)" runs/kv$r-*.out 2>/dev/null | wc -l) -ge 4 ]; do sleep 15; done
done
echo klatv-done > runs/klatv.done
