#!/bin/bash
# klane.sh IMG(name) SMP PINS MEM...: KVM boots one after the other until "interactive loop started"
cd /srv/euroos/integ2-runs/m430; T=$1 SMP=$2 PIN=$3; shift 3
mkdir -p kvm/$T
for M in "$@"; do ./kboot.sh img/$T.img kvm/$T s$SMP-$M $M $SMP $PIN "interactive loop started" 900 120 > kvm/$T/s$SMP-$M.res 2>&1; done
touch kvm/$T/s$SMP.done
