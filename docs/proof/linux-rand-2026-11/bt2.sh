#!/usr/bin/env bash
# Boot time of the public image once more, when none of our other runs is
# active on the host: base (b9a5755) and final, alternating, 10 boots each.
B=/srv/euroos/rand-runs; S=$B/scripts
until [ -f $B/ab.done ] && [ -f $B/short.done ] && ! pgrep -f "$B/short-boots.sh" >/dev/null; do sleep 20; done
mkdir -p $B/bt2
for i in 1 2 3 4 5 6 7 8 9 10; do
  python3 $S/boot-time.py base-$i $B/base-pub.img $B/bt2 --pin 38-39 --hold 5
  python3 $S/boot-time.py final-$i $B/after-pub.img $B/bt2 --pin 38-39 --hold 5
done > $B/bt2/kvm-ab.txt 2>&1
rm -f $B/bt2/*.d/boot.img
echo "done $(date +%T)" > $B/bt2.done
