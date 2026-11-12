#!/usr/bin/env bash
# When the soak has ended (its 4 vCPUs are free): two more lanes of short boots
# on its CPUs, until the four lanes together have 100 boots.
B=/srv/euroos/rand-runs
cd $B
until grep -q "completed clean\|\[loadtest\] .* results" soak/out.txt 2>/dev/null && ! pgrep -f "workdir $B/soak/w" >/dev/null; do sleep 20; done
(setsid nohup ./short-boots.sh $B/after-st.img $B short-c 40 4-5 kvm host "[linux-rand] getrandom" 600 2 > /dev/null 2>&1 < /dev/null &)
(setsid nohup ./short-boots.sh $B/after-st.img $B short-d 40 6-7 kvm host "[linux-rand] getrandom" 600 2 > /dev/null 2>&1 < /dev/null &)
until [ "$(cat short-*/summary.txt 2>/dev/null | grep -c "^boot=")" -ge 100 ]; do sleep 10; done
touch $B/STOP-SHORT
echo "100 reached $(date +%T)" > $B/short.done
