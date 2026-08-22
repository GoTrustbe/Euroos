#!/usr/bin/env python3
"""Map a sampled instruction pointer back to a symbol in a PIE binary.

The kernel's RIP profiler prints `exe+OFFSET` for the main thread's samples; a PIE is
loaded at a base the kernel chooses, so that offset IS the address inside the ELF.
Only dynamic symbols survive in a release chrome, so the answer is "the nearest
exported symbol at or before this address" — enough to name the region a spin is in.

Usage: rip2sym.py BINARY OFFSET [OFFSET...]      (offsets in hex, with or without 0x)
"""
import subprocess, sys

binary = sys.argv[1]
# Prefer the FULL symbol table when the binary still has one (a release chrome does,
# and it names internal functions that the dynamic table never exports); fall back to
# the dynamic symbols for a stripped binary.
out = subprocess.run(["nm", "--defined-only", "-C", binary],
                     capture_output=True, text=True).stdout
if len(out) < 100:
    out = subprocess.run(["nm", "-D", "--defined-only", "-C", binary],
                         capture_output=True, text=True).stdout
syms = []
for line in out.splitlines():
    parts = line.split(None, 2)
    if len(parts) == 3 and parts[0].strip():
        try:
            syms.append((int(parts[0], 16), parts[2]))
        except ValueError:
            pass
syms.sort()
for arg in sys.argv[2:]:
    off = int(arg, 16)
    lo, hi, best = 0, len(syms) - 1, None
    while lo <= hi:
        mid = (lo + hi) // 2
        if syms[mid][0] <= off:
            best = syms[mid]; lo = mid + 1
        else:
            hi = mid - 1
    if best:
        print(f"{off:#x}  {best[1]} + {off - best[0]:#x}")
    else:
        print(f"{off:#x}  (before the first dynamic symbol)")
