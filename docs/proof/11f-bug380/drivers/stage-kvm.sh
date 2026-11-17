#!/bin/bash
# 30 KVM boots of B under sibling load ([vdso-mono]) + markers boots B and base (1/2/4 GiB x 1/2/4 vCPU, quiet)
B=/srv/euroos/b380-runs; cd $B; mkdir -p kvm30 kvmm
M="child has a vDSO"
( for i in 1 2 3 4 5 6 7 8 9 10; do ./loadrun.sh 36,37 ./kboot.sh $B/img/b-st.img $B/kvm30 l2-$i 2G 2 16-17 "$M" 900 > kvm30/l2-$i.res 2>&1; done ) &
( for i in 1 2 3 4 5 6 7 8 9 10; do ./loadrun.sh 38,39 ./kboot.sh $B/img/b-st.img $B/kvm30 l4-$i 2G 4 18,19,38,39 "$M" 900 > kvm30/l4-$i.res 2>&1; done ) &
wait
lane() { P=$1; shift; for j in "$@"; do set -- $(echo $j | tr : " "); ./kboot.sh $B/img/$1-st.img $B/kvmm $1-$2-$3 $2 $3 $P "interactive loop started" 900 > kvmm/$1-$2-$3.res 2>&1; done; }
lane 16 b:1G:1 b:2G:1 b:4G:1 base:1G:1 base:2G:1 base:4G:1 &
lane 17,37 b:1G:2 b:2G:2 b:4G:2 &
lane 36,38 base:1G:2 base:2G:2 base:4G:2 &
wait
lane 16,17,36,37 b:1G:4 b:2G:4 b:4G:4 &
lane 18,19,38,39 base:1G:4 base:2G:4 base:4G:4 &
wait; touch kvm.done
