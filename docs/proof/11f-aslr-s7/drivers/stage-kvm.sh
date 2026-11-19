#!/bin/bash
# stage-kvm.sh: KVM self-test boots 1/2/4 GiB, 2 vCPU, HD-Audio, quiet host, one lane per image, each on one core + its sibling.
# m = merge (4599d0e), s6 = 55c6e25, b380 = 19f7586. Marker: the desktop loop.
B=/srv/euroos/aslr7-runs; cd $B; mkdir -p kvm
lane() { T=$1 PIN=$2; for MEM in 1G 2G 4G; do
    ./kboot.sh $B/img/$T-st.img $B/kvm $T-$MEM $MEM 2 $PIN "interactive loop started" 900 > kvm/$T-$MEM.res 2>&1
  done; touch kvm/$T.done; }
lane "$1" "$2"
