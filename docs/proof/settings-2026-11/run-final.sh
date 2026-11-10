#!/usr/bin/env bash
# T2 final proof runs (own work dirs under /opt/euroos/t2-runs, own QMP sockets).
set -u
cd /opt/euroos/t2-runs
V=${1:-v3}
S=/opt/euroos/wt-settings/scripts
rm -rf tcg-$V live-$V wiz-$V pA-$V pB-$V
mkdir -p tcg-$V
# 1. Selftest image, TCG, user smp4 (IOMMU) and smp1 (no IOMMU), side by side.
( PIN=4-7 ./boot.sh user-smp4-iommu 512M 4 1 user selftest-$V.img tcg-$V > tcg-$V/q-a.txt 2>&1 ) &
( PIN=2-3 ./boot.sh user-smp1-noiommu 512M 1 0 user selftest-$V.img tcg-$V > tcg-$V/q-b.txt 2>&1 ) &
# 2. Public image, live session: every section + the settings in use.
python3 t2-gui.py live-$V public-$V.img live.txt > live-$V.out 2>&1
# 3. Public image installed by the wizard onto a 4 GiB disk, then two boots of that disk.
python3 $S/wizard-install-test.py public-$V.img wiz-$V > wiz-$V.out 2>&1
cp wiz-$V/target.img installed-$V.img
python3 t2-gui.py pA-$V installed-$V.img persist-a.txt --boot-disk --mem 2G --wait-marker "[ag-lock]" > pA-$V.out 2>&1
python3 t2-gui.py pB-$V installed-$V.img persist-b.txt --boot-disk --mem 2G --wait-marker "[ag-lock]" > pB-$V.out 2>&1
wait
echo "T2-FINAL-DONE $(date +%T)" > final-$V.done
