#!/bin/bash
# R630 (KVM), after the 204 entropy boots: the BUG-063 scenario from the desktop Terminal on the base image
# and on the final one, then the boot time of the public image (base, before the BUG-065 fix, final;
# alternating on the same two CPUs). QEMU only with -nic none or restrict=on.
set -u
B=/srv/euroos/integ2-runs; S=$B/s/scripts; U=$B/s/docs/proof/uidebug-2026-11
cd $B
until [ "$(cat after5-*/summary.txt 2>/dev/null | grep -c '^done')" -ge 6 ]; do sleep 20; done

# 1. BUG-063 from the desktop: /bin/hello in the Terminal, then the userspace driver e1000d works (ping).
NICX="-netdev user,id=n0,restrict=on -device e1000e,netdev=n0"
for img in pubbase pub5; do
  for i in 1 2 3 4 5; do
    D=$B/bug063-$img-$i
    python3 $U/probe.py $S/gui-smoke.py $B/$img.img $D $B/bug063.txt --kvm --pin 12-15 --smp 4 --mem 4G --extra "$NICX" > $D.out 2>&1
    rc=$?
    L=$(ls $D/*.log 2>/dev/null | grep -i serial | head -1)
    crash=$(grep -aE "KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT|\[idt\] PAGE FAULT|\[idt\] INVALID OPCODE" "$L" 2>/dev/null | tr -d '\r' | head -2 | cut -c1-200)
    echo "$img run $i: probe rc=$rc; hello lines on the Terminal: $(grep -ac '^\[e2e\] \$ hello' "$L"); ping answers: $(grep -ac '^\[e2e\] PING.*echo-reply OK' "$L"); crash lines: ${crash:-none}" >> $B/bug063-desktop.txt
    rm -f $D/*.img
  done
done
echo done > $B/bug063.done

# 2. Boot time, 5 boots each, alternating.
mkdir -p $B/bt
for i in 1 2 3 4 5; do
  python3 $S/boot-time.py base-$i $B/pubbase.img $B/bt --pin 38-39 --hold 5
  python3 $S/boot-time.py prefix-$i $B/pubpre.img $B/bt --pin 38-39 --hold 5
  python3 $S/boot-time.py final-$i $B/pub5.img $B/bt --pin 38-39 --hold 5
done > $B/bt/kvm-ab.txt 2>&1
for i in 1 2 3 4 5; do
  python3 $S/boot-time.py prefix-modern-$i $B/pubpre.img $B/bt --mode modern --pin 38-39 --hold 5
  python3 $S/boot-time.py final-modern-$i $B/pub5.img $B/bt --mode modern --pin 38-39 --hold 5
done >> $B/bt/kvm-ab.txt 2>&1
echo done > $B/bt.done
