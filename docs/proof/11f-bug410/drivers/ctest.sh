#!/bin/bash
export PATH=/root/.cargo/bin:$PATH CARGO_TARGET_DIR=/opt/euroos/integ2b410-target/host
W=/opt/euroos/integ2b410-work
for t in d18fa36 fix; do cd $W/src/$t; nice -n 10 cargo test -j 4 > $W/ctest-$t.log 2>&1; echo "$t rc=$?" >> $W/ctest-status.txt; done
touch $W/ctest.done
