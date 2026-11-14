#!/bin/bash
# three rename-persist runs in parallel, image $1, tag $2
cd /srv/euroos/kbugs-runs; S=$PWD/scripts
i=0
for p in 8-9 10-11 12-13; do i=$((i+1)); d=rp-$2-$i; rm -rf $d; mkdir -p $d
  (python3 $S/rename-persist.py --image img/$1 --workdir $PWD/$d --wizard $S/wizard-install-test.py --kvm --pin $p > $d.out 2>&1; rm -f $d/target.img; echo done > $d.done) &
done
wait
