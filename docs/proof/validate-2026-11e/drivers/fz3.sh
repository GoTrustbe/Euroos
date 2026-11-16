#!/bin/bash
cd /srv/euroos/val-runs; B=$PWD; S=$B/scripts
until [ $(ps -eo args | grep -c "[s]hort-boots.sh.*fz2") -eq 0 ]; do sleep 10; done
for p in $(cat fz2/noise.pids); do if [ -n "$p" ] && [ "$p" -gt 1 ] && tr "\0" " " < /proc/$p/cmdline | grep -q "while :; do :; done"; then kill $p; fi; done
mkdir -p fz3
for x in "hv1a $B/img/v1-st.img 14-15" "hv1b $B/img/v1-st.img 16-17" "hd11a /srv/euroos/final-runs/img/st.img 34-35" "hd11b /srv/euroos/final-runs/img/st.img 36-37" "hx3a $B/img/x3-st.img 4-5" "hb8a /srv/euroos/d2-runs/img/b8-st.img 24-25"; do set -- $x; (setsid nohup bash $S/short-boots.sh $2 $B/fz3 $1 16 $3 kvm host "[fuzz] Linux-ABI" 600 2 > /dev/null 2>&1 < /dev/null &); done
