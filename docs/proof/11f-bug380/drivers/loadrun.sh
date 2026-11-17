#!/bin/bash
# loadrun.sh LOADPINS CMD...: busy loops on LOADPINS (comma list) while CMD runs; the loops are stopped by PID.
LP=$1; shift
PIDS=()
for c in ${LP//,/ }; do taskset -c $c sh -c "while :; do :; done" & PIDS+=($!); done
"$@"
for p in "${PIDS[@]}"; do if [ -n "$p" ] && [ "$p" -gt 1 ]; then kill $p; fi; done
wait 2>/dev/null
