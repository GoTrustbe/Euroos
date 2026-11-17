#!/bin/bash
# stress.sh SMP PINS FROM TO: KVM boots of fix-st.img up to the [swap-irq] line (fork children with threads beside a kernel task that takes DEMAND_FILE_MAPS with interrupts on).
B=/srv/euroos/b390-runs; cd $B; SMP=$1 PINS=$2
mkdir -p runs/stress
for i in $(seq $3 $4); do ./kboot.sh img/fix-st.img runs/stress s$SMP-$i 4G $SMP $PINS "[swap-irq]" 900 120 >> runs/stress/summary.txt 2>&1; done
