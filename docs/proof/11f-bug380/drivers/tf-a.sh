#!/bin/bash
# tests first, image A (test + diagnosis on 7b2eea6): KVM 1/2/4 vCPU with busy siblings, then TCG 1/2/4 at once
B=/srv/euroos/b380-runs; cd $B; I=$B/img/$1; O=$B/tf/$2; mkdir -p $O
M="child has a vDSO"
./loadrun.sh 36 ./kboot.sh $I $O kvm-1 2G 1 16 "$M" 900 > $O/kvm-1.res 2>&1
./loadrun.sh 36,37 ./kboot.sh $I $O kvm-2 2G 2 16-17 "$M" 900 > $O/kvm-2.res 2>&1
./loadrun.sh 36,37,38,39 ./kboot.sh $I $O kvm-4 2G 4 16-19 "$M" 900 > $O/kvm-4.res 2>&1
export ACCEL="-cpu qemu64,+smep,+smap"
./kboot.sh $I $O tcg-1 2G 1 16 "$M" 3600 > $O/tcg-1.res 2>&1 &
./kboot.sh $I $O tcg-2 2G 2 17,37 "$M" 3600 > $O/tcg-2.res 2>&1 &
./kboot.sh $I $O tcg-4 2G 4 18,19,38,39 "$M" 3600 > $O/tcg-4.res 2>&1 &
wait
touch $O/done
