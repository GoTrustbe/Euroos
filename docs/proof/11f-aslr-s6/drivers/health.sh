#!/bin/bash
# health.sh RUN...: per Chromium run: verdict line, [abort] lines, ring-0 faults/panics/watchdog, FROZEN, [oom], exec lines, arena mmap window full
cd /srv/euroos/aslr6-runs
for N in "$@"; do
  L=$(ls runs/$N/*/serial.log runs/$N/serial*.log runs/$N/*.log 2>/dev/null | sort -u)
  v=$(grep -a "^\[chrome-smoke\] \(PASS\|FAIL\|CRASH\)\|starts: \|^\[chrome-mem-soak\] \(PASS\|FAIL\|CRASH\)" runs/$N.out | tail -n 1 | cut -c1-110)
  echo "## $N: $v | aborts=$(cat $L 2>/dev/null | grep -ac "^\[abort\]") ring0=$(cat $L 2>/dev/null | grep -aEc "KERNEL PANIC|GENERAL PROTECTION|\[idt\] PAGE FAULT|DOUBLE FAULT|panicked at|WATCHDOG TRIPPED") FROZEN=$(cat $L 2>/dev/null | grep -ac FROZEN) oom=$(cat $L 2>/dev/null | grep -ac "^\[oom\]") execs=$(cat $L 2>/dev/null | grep -ac "^\[aslr-layout\] exec") window-full=$(cat $L 2>/dev/null | grep -ac "mmap window full") ring3-kills=$(cat $L 2>/dev/null | grep -ac "ended by signal")"
done
