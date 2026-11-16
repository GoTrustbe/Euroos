#!/bin/bash
# psum.sh RUN...: keylat median, t4 share, kernel total, serial-tx line
cd /srv/euroos/reval-runs/runs
for r in "$@"; do echo "== $r $(grep -a "key to screen" $r.out | grep -o "median [0-9]* ms")  t4 $(grep -a "t4 (kernel" $r/serial.log | tail -n 1 | grep -o "ran [0-9.]* %")  kern $(grep -a "keylat\]   total" $r/serial.log | tail -n 1 | grep -o "median [0-9]*")  faults $(grep -a -o "faults-total=[0-9]*" $r/serial.log | tail -n 1)"; done
python3 ../if0sum.py $(for r in "$@"; do echo $r/serial.log; done)
