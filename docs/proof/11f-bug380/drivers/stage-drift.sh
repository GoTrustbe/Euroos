#!/bin/bash
# drift of the vDSO clock against the host over 12 min, A (7b2eea6 + test, old clock) and B (fix) side by side
B=/srv/euroos/b380-runs; cd $B; mkdir -p drift
python3 vdrift.py img/a-st.img drift/kvm-a 16,17 2 2G 720 > drift/kvm-a.txt 2>&1 &
python3 vdrift.py img/b-st.img drift/kvm-b 18,19 2 2G 720 > drift/kvm-b.txt 2>&1 &
python3 vdrift.py img/a-st.img drift/tcg-a 36 1 2G 720 tcg > drift/tcg-a.txt 2>&1 &
python3 vdrift.py img/b-st.img drift/tcg-b 38 1 2G 720 tcg > drift/tcg-b.txt 2>&1 &
wait; touch drift.done
