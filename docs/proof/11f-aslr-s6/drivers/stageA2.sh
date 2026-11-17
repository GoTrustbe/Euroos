#!/bin/bash
# stageA2.sh: after the TCG matrix: 60 short TCG boots to [fuzz] (6 lanes of 10, 2 vCPU, smep+smap); after the entropy
# boots: installs (wizard, ro-install plain/inject/ro0). CPUs 20-35 only (0-15/20-35 allowed; 16-19, 36-39 not used).
B=/srv/euroos/aslr6-runs; cd $B; S=$B/scripts; I=$B/img
until [ -f tcg.done ] && [ $(ps -eo args | grep -c "[t]cgq.sh") -eq 0 ]; do sleep 30; done
mkdir -p short
for p in 20 22 24 26 28 34; do
  (bash $S/short-boots.sh $I/s6-st.img $B/short on-l$p 10 $p-$((p+1)) tcg qemu64,+smep,+smap "[fuzz] Linux-ABI" 1500 2 > /dev/null 2>&1 < /dev/null &)
done
until [ $(ps -eo args | grep -c "[e]nt-boots.sh") -eq 0 ]; do sleep 30; done
PIN=30-33
taskset -c $PIN python3 $S/wizard-install-test.py $I/s6-pub.img $B/wizard-on > wizard-on.out 2>&1 < /dev/null
for m in plain inject ro0; do mkdir -p roi-on-$m; MODE=$m python3 $S/ro-install-test.py $I/s6-st.img roi-on-$m $PIN > roi-on-$m.out 2>&1 < /dev/null; done
rm -f roi-on-*/*.img wizard-on/*.img 2>/dev/null
touch installs.done
sleep 60; until [ $(ps -eo args | grep -c "[s]hort-boots.sh") -eq 0 ]; do sleep 30; done
touch short.done
