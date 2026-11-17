#!/bin/bash
B=/srv/euroos/aslr5-runs; cd $B
bash scripts/short-boots.sh img/s5-st.img ent s5a 10 16-17 kvm host "[aslr-exec]" 900 2 > ent/s5a.out 2>&1 &
bash scripts/short-boots.sh img/s5-st.img ent s5b 10 30-31 kvm host "[aslr-exec]" 900 2 > ent/s5b.out 2>&1 &
bash scripts/short-boots.sh img/m-st.img ent ma 10 18-19 kvm host "[aslr-exec]" 900 2 > ent/ma.out 2>&1 &
bash scripts/short-boots.sh img/m-st.img ent mb 10 32-33 kvm host "[aslr-exec]" 900 2 > ent/mb.out 2>&1 &
wait; touch ent.done
