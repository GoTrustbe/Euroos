#!/bin/bash
# cmp.sh BASE NEW: markers-compare for every (smp, mem) boot
cd /srv/euroos/integ2-runs/m430
for v in 1 2 4; do for m in 1G 2G 4G; do echo "== kvm/$1/s$v-$m.log  vs  kvm/$2/s$v-$m.log"; python3 scripts/markers-compare.py compare kvm/$1/s$v-$m.log kvm/$2/s$v-$m.log 2>&1 | grep -v "^== "; done; done
