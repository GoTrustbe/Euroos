#!/bin/bash
cd /srv/euroos/reval-runs/q35p; until [ -f ../laneB.done ]; do sleep 20; done
EK_RESTRICT=1 EK_ACCEL=kvm taskset -c 18-19 python3 ../scripts/run-metal-matrix.py $PWD/st.raw --legs printer > matrix.out 2>&1; touch ../pr.done
