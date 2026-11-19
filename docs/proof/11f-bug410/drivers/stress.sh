#!/bin/bash
# stress.sh IMAGE-NAME SMP PINS FROM TO: KVM boots of a self-test image up to the [swap-irq] line, results in runs/stress-IMAGE/summary-SMP.txt
cd /srv/euroos/integ2-b410-runs; T=$1 SMP=$2 PINS=$3
mkdir -p runs/stress-$T
for i in $(seq $4 $5); do ./kboot.sh img/$T.img runs/stress-$T s$SMP-$i 4G $SMP $PINS "[swap-irq]" 900 120 >> runs/stress-$T/summary-$SMP.txt 2>&1; done
