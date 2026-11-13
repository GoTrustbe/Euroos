#!/bin/bash
# usage: gtk-loop.sh IMAGE TAG N PINS : the live GTK demo N times; stops at the first run in which the demo test ran
B=/srv/euroos/validate-runs
for i in $(seq 1 $3); do
  rm -rf $B/$2-$i
  python3 $B/scripts/gtk-live.py $B/$1 $B/$2-$i --kvm --pin $4 > $B/$2-$i.out 2>&1
  first=$(grep -a -m1 "file-backed ->" <(sed -n "/ggtk live on task/,\$p" $B/$2-$i/serial.log) | sed -E "s/.*-> (0x[0-9a-f]+).*/\1/")
  echo "run $i: first library mapping at $first; $(grep -c 'GGTK: gtk_init ok' $B/$2-$i.out) init, $(grep -c 'failed to map segment' $B/$2-$i.out) load failures, $(grep -c 'gtk-test' $B/$2-$i.out) demo-test lines, $(grep -c 'GGTK: KEY\|GGTK: entry\|GGTK: BUTTON' $B/$2-$i.out) input lines of the app" >> $B/$2-loop.txt
  grep -q "gtk-test" $B/$2-$i.out && [ "$5" != all ] && break
  [ "$5" = all ] || rm -rf $B/$2-$i
done
echo done >> $B/$2-loop.txt
