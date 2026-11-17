#!/bin/bash
cd /srv/euroos/b380-runs
until [ -e tf/b/done ]; do sleep 20; done
./stage-tcg.sh; ./stage-kvm.sh; ./stage-drift.sh; ./stage-ui.sh
touch final.done
