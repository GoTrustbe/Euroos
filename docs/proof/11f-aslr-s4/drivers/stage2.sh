#!/bin/bash
# stage2: 60 short TCG boots per mode until [fuzz] Linux-ABI, 2 vCPU, both SMT siblings of every core busy
# (lanes on cores 4-15 and their siblings 24-35); ON lanes on even pairs, OFF lanes on odd pairs; 10 boots per lane.
B=/srv/euroos/aslr4-runs; cd $B; S=$B/scripts; mkdir -p short
k=0
for p in 4 6 8 10 12 14; do for q in $p $((p+20)); do
  if [ $((k % 2)) = 0 ]; then M=on; IMG=$B/img/s4a-st-fix.img; else M=off; IMG=$B/img/s4a-st.img; fi
  (bash $S/short-boots.sh $IMG $B/short/$M l$q 10 $q-$((q+1)) tcg qemu64,+smep,+smap "[fuzz] Linux-ABI" 1500 2 > /dev/null 2>&1 < /dev/null &)
  k=$((k+1))
done; done
sleep 60; until [ $(ps -eo args | grep -c "[s]hort-boots.sh") -eq 0 ]; do sleep 15; done
touch short.done
