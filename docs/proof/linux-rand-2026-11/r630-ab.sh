#!/usr/bin/env bash
# What the CSPRNG costs on the process start path: the same shell commands on
# the self-test image before the fix (91afd7d) and after it, alternating, on
# the same four CPUs. `churn proc 8` starts 8 glibc processes (gtiny) and reaps
# them; `spawn worker 32` starts 32 musl background processes.
# R630, KVM, SLIRP restrict=on. usage: r630-ab.sh (starts in the background)
B=/srv/euroos/rand-runs; S=$B/scripts
CMDS=""
for i in $(seq 1 40); do CMDS="${CMDS}churn proc 8;"; done
for i in $(seq 1 10); do CMDS="${CMDS}spawn worker 32;"; done
CMDS="${CMDS}ltstat"
(
  until [ -f $B/chain2.done ]; do sleep 20; done
  for r in 1 2 3; do
    for v in before after; do
      W=$B/ab/$v-$r; mkdir -p $W
      timeout 1800 python3 $S/loadtest.py --image $B/$v-st.img --workdir $W --kvm --smp 4 --mem 4G --pin 12-15 \
        --nic e1000e --restrict --nmi-on-hang --boot-timeout 900 --scenario cmds --cmds "$CMDS" --csv $W/results.csv > $W/out.txt 2>&1
      echo "$v-$r rc=$? $(date +%T)" >> $B/ab/done.txt
      rm -f $W/*.img $W/*.raw
    done
  done
  echo "done $(date +%T)" > $B/ab.done
) > /dev/null 2>&1 < /dev/null &
disown -a
echo started ab
