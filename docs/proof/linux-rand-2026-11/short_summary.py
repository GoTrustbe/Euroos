#!/usr/bin/env python3
"""Summary of the short boots (short-boots.sh): verdict counts, the [entropy]
line of every boot, and whether any AT_RANDOM, stack guard, getrandom or
device value occurs twice over all processes of all boots.
usage: short_summary.py DIR [DIR ...]   (each DIR holds summary.txt and lines-N.txt)"""
import glob
import os
import re
import statistics
import sys

KEYS = ("at_random", "stack_guard", "getrandom", "urandom", "random")
boots = []
values = {k: {} for k in KEYS}
for d in sys.argv[1:]:
    for line in open(os.path.join(d, "summary.txt"), encoding="utf-8", errors="replace"):
        if line.startswith("boot="):
            boots.append((d, line.rstrip("\n")))
    for f in glob.glob(os.path.join(d, "lines-*.txt")):
        for line in open(f, encoding="utf-8", errors="replace"):
            m = re.match(r"^\[linux-rand\]\s+(\S+) randprobe: (\w+)=([0-9a-f]+)", line)
            if m and m.group(2) in values:
                values[m.group(2)].setdefault(m.group(3), []).append(f"{os.path.basename(d)}/{os.path.basename(f)}/{m.group(1)}")


def count(pat):
    return sum(1 for _, l in boots if re.search(pat, l))


n = len(boots)
print(f"boots: {n}  (by lane: " + ", ".join(f"{os.path.basename(d)} {sum(1 for x, _ in boots if x == d)}" for d in sys.argv[1:]) + ")")
print(f"ended at the marker: {count(r'end=marker')}, timeout: {count(r'end=timeout')}, qemu exit: {count(r'end=exit')}")
for t in ("3d8", "3d7", "3d3"):
    ok, bad = count(t + " ok=1 failed=0"), count(t + " ok=[0-9]+ failed=[1-9]")
    print(f"[{t}]: OK in {ok}, FAILED in {bad}")
one, seeded, unseeded = count("entropy lines=1 "), count("seeded=1 not-seeded=0"), count("not-seeded=[1-9]")
print(f"[entropy]: one status line in {one}, seeded in {seeded}, NOT SEEDED in {unseeded}")
ok, bad = count("linux-rand ok=1 failed=0"), count("linux-rand ok=[0-9]+ failed=[1-9]")
print(f"[linux-rand]: OK in {ok}, FAILED in {bad}")
crash = count("crash-lines=[1-9]")
print(f"boots with a crash line: {crash}")
fl = {}
for _, l in boots:
    m = re.search(r"failed-lines=(\d+)", l)
    if m:
        fl[m.group(1)] = fl.get(m.group(1), 0) + 1
print(f"FAILED lines per boot (any tag, up to the marker): {fl}")
bits, ms, secs, batches = [], [], [], []
for _, l in boots:
    m = re.search(r"CSPRNG seeded: (\d+) of 256 bits", l)
    if m:
        bits.append(int(m.group(1)))
    m = re.search(r"jitter=(\d+) batches", l)
    if m:
        batches.append(int(m.group(1)))
    m = re.search(r"· ([0-9.]+) ms →", l)
    if m:
        ms.append(float(m.group(1)))
    m = re.search(r"secs=([0-9.]+)", l)
    if m:
        secs.append(float(m.group(1)))
if bits:
    print(f"credited bits: min {min(bits)}, median {statistics.median(bits):.0f}, max {max(bits)}; jitter batches: {sorted(set(batches))}")
if ms:
    print(f"entropy::init: min {min(ms):.3f} ms, median {statistics.median(ms):.3f} ms, max {max(ms):.3f} ms")
if secs:
    print(f"seconds to the marker: min {min(secs):.0f}, median {statistics.median(secs):.0f}, max {max(secs):.0f}")
for k in KEYS:
    total = sum(len(v) for v in values[k].values())
    dup = {v: w for v, w in values[k].items() if len(w) > 1}
    print(f"{k}: {total} values over all processes of all boots, {len(values[k])} distinct, {len(dup)} seen more than once")
    for v, w in list(dup.items())[:5]:
        print(f"   {v}: {w}")
