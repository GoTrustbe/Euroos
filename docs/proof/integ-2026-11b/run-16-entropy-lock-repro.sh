#!/bin/bash
# .16 (TCG), after chain A: 20 entropy boots, the lock screen frame under TCG, and (after chain B) repro-build.sh.
export PATH=$HOME/.cargo/bin:$PATH
R=/opt/euroos/integ2-runs; W=/opt/euroos/wt-integ2
cd $R
until [ -f $R/tcg/matrix.done ]; do sleep 20; done
./eboots.sh $R/build/st5.img $R tcg-entropy-a 10 8-11 tcg "qemu64,+smep,+smap" "[3d3]" 2400 &
./eboots.sh $R/build/st5.img $R tcg-entropy-b 10 12-15 tcg "qemu64,+smep,+smap" "[3d3]" 2400 &
wait
echo "done $(date +%T)" > $R/tcg-entropy.done
mkdir -p $R/lock
cp --sparse=always $R/build/st5.img $R/lock/st2.img
python3 $W/scripts/boot-time.py integ-tcg-shot $R/lock/st2.img $R/lock --accel tcg --smp 4 --mem 512M --marker "[ag-lock] GUI" --hold 0 --shot $R/lock/lock-integ-tcg.png --timeout 1800 --extra "-rtc base=2026-11-12T10:00:50" > $R/lock/integ-tcg-shot.out 2>&1
rm -f $R/lock/st2.img
echo "done $(date +%T)" > $R/lock.done
until [ -f $R/chainB.done ]; do sleep 20; done
bash $R/repro.sh > $R/proof/repro-build.txt 2>&1
echo "done $(date +%T)" > $R/chainC.done
