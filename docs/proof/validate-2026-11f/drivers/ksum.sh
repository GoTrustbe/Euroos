#!/bin/bash
# ksum.sh: one line per keylat run: fbread median, screendump median, kernel [keylat] total median (last 30 keys)
cd /srv/euroos/v11f-runs/runs
for f in ${KP:-kl}*-*.out; do n=${f%.out}
  fb=$(grep -a "keys, fbread: median" $f | grep -o "median [0-9]*" | head -1 | cut -d" " -f2)
  sd=$(grep -a "screendump method" $f | grep -o "(until 11e): median [0-9]*" | grep -o "[0-9]*$" | head -1)
  kt=$(grep -a "^\[keylat\]   total: median" $n/serial.log | tail -n 1 | grep -o "median [0-9]*" | cut -d" " -f2)
  echo "$n fbread=$fb screendump=$sd kernel_us=$kt"
done
