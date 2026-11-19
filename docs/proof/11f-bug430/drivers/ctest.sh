#!/bin/bash
export PATH=/root/.cargo/bin:$PATH CARGO_TARGET_DIR=/opt/euroos/integ2m-target/host
cd /opt/euroos/integ2m-work/src/$1; nice -n 10 cargo test -j 2 > /opt/euroos/integ2m-work/ctest-$1.log 2>&1; echo "$1 rc=$?" >> /opt/euroos/integ2m-work/ctest-status.txt; touch /opt/euroos/integ2m-work/ctest-$1.done
