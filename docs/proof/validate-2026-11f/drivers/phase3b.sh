#!/bin/bash
# phase3b.sh: 108 short boots, 12 lanes on cores 4-9 and 14-19 AND their siblings (both siblings of every core busy), 9 boots per lane
B=/srv/euroos/v11f-runs; cd $B; S=$B/scripts
mkdir -p short
for p in 4 6 8 12 14 16; do for q in $p $((p+20)); do
  (bash $S/short-boots.sh $B/img/m-st.img $B/short l$q 9 $q-$((q+1)) kvm host "[fuzz] Linux-ABI" 600 2 > /dev/null 2>&1 < /dev/null &)
done; done
sleep 60; until [ $(ps -eo args | grep -c "[s]hort-boots.sh") -eq 0 ]; do sleep 15; done
touch short.done
