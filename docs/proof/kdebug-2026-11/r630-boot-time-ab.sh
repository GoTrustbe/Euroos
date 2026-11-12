#!/usr/bin/env bash
# Release image boot time on the R630: base 7858b72 and the final image alternating on the same two CPUs,
# so that both see the same host load. Then the lock screen of the final self-test image (timing and frame).
B=/srv/euroos/kdebug-runs; S=$B/scripts; cd $B
mkdir -p bt3 lock3
for i in 1 2 3 4 5; do
  python3 $S/boot-time.py base-$i $B/base-image.img $B/bt3 --pin 38-39 --hold 5
  python3 $S/boot-time.py final-$i $B/image-final.img $B/bt3 --pin 38-39 --hold 5
done > $B/bt3/kvm-ab.txt 2>&1
for i in 1 2 3 4 5; do
  python3 $S/boot-time.py base-modern-$i $B/base-image.img $B/bt3 --mode modern --pin 38-39 --hold 5
  python3 $S/boot-time.py final-modern-$i $B/image-final.img $B/bt3 --mode modern --pin 38-39 --hold 5
done >> $B/bt3/kvm-ab.txt 2>&1
python3 $S/boot-time.py final-kvm selftest-final.img lock3 --smp 2 --mem 2G --pin 38-39 --hold 3 --timeout 900 > lock3/final-kvm.out 2>&1
python3 $S/boot-time.py final-kvm-shot selftest-final.img lock3 --smp 2 --mem 2G --pin 38-39 --marker "[ag-lock] GUI" --hold 0 --shot lock3/final-kvm-lock.png --timeout 900 --extra "-rtc base=2026-11-12T10:00:00" > lock3/final-kvm-shot.out 2>&1
echo done > $B/bt3/kvm-ab.done
