#!/bin/bash
# klane.sh PINS IMAGE-NAME MEM...: KVM boots one after the other, 2 vCPU, until "interactive loop started"
cd /srv/euroos/integ2-b410-runs; PIN=$1 T=$2; shift 2
mkdir -p runs/kvm
for M in "$@"; do ./kboot.sh img/$T.img runs/kvm/$T k-$M $M 2 $PIN "interactive loop started" 900 120 > runs/kvm/$T-$M.res 2>&1; done
