#!/bin/bash
# stage2: 60 short TCG boots of the S5 self-test image to [fuzz] Linux-ABI, 2 vCPU, smep+smap, 6 lanes of 10
B=/srv/euroos/aslr5-runs; cd $B; mkdir -p short
for p in 16 18 30 32 34 36; do
  (bash scripts/short-boots.sh $B/img/s5-st.img $B/short on-l$p 10 $p-$((p+1)) tcg qemu64,+smep,+smap "[fuzz] Linux-ABI" 1500 2 > /dev/null 2>&1 < /dev/null &)
done
sleep 60; until [ $(ps -eo args | grep -c "[s]hort-boots.sh") -eq 0 ]; do sleep 15; done
touch short.done
