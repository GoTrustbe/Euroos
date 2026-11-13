#!/bin/bash
# usage: gtk-diag.sh IMAGE TAG N PINS : the live GTK demo N times, each run to the desktop, with screenshots and cdiag
B=/srv/euroos/validate-runs
for i in $(seq 1 $3); do
  rm -rf $B/$2-$i
  python3 $B/scripts/drive.py $B/$1 $B/$2-$i --kvm --pin $4 -- "wait:ggtk live on task:600" "wait:interactive loop started:600" sleep:6 shot:desk-a scon:cdiag sleep:4 shot:desk-b "scon:xtrace on" sleep:4 "scon:xtrace off" sleep:20 shot:desk-c > $B/$2-$i.out 2>&1
  L=$B/$2-$i/serial.log
  first=$(sed -n "/ggtk live on task/,\$p" $L | grep -a -m1 "file-backed ->" | sed -E "s/.*-> (0x[0-9a-f]+).*/\1/")
  echo "run $i: first library mapping at $first; gtk_init $(grep -ac 'app\]   GGTK: gtk_init ok' $L), window shown $(grep -ac 'app\]   GGTK: window shown' $L), load error: $(grep -a 'app\]   ggtk: error' $L | head -1 | sed -E 's/.*libraries: //' | cut -c1-80), live ticks $(grep -ac 'GGTK: live tick' $L), gtk-test lines $(grep -ac '^\[gtk-test\]' $L), app input lines $(grep -ac 'GGTK: KEY\|GGTK: entry\|GGTK: BUTTON' $L)" >> $B/$2-loop.txt
done
echo done >> $B/$2-loop.txt
