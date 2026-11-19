#!/bin/bash
# stress.sh IMG SMP PINS FROM TO: KVM boots of a self-test image up to the [swap-irq] line, 4 GiB
cd /srv/euroos/integ2-runs/m430; T=$1 SMP=$2 PINS=$3
mkdir -p runs/stress-$T
for i in $(seq $4 $5); do ./kboot.sh img/$T.img runs/stress-$T s$SMP-$i 4G $SMP $PINS "[swap-irq]" 900 120 >> runs/stress-$T/summary-$SMP.txt 2>&1; done
touch runs/stress-$T/done-$SMP-$4
