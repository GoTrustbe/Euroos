#!/usr/bin/env python3
"""Summary of eboots.sh runs. usage: entropy_summary.py TITLE DIR [DIR...]
Reads DIR/summary.txt and DIR/lines-N.txt (the [3d8], [3d7], [3d3], [entropy] lines of each boot)."""
import glob, os, re, statistics as st, sys

title, dirs = sys.argv[1], sys.argv[2:]
boots = ends = 0
ok = {"3d8": 0, "3d7": 0, "3d3": 0}
failed = {"3d8": 0, "3d7": 0, "3d3": 0}
missing = {"3d8": 0, "3d7": 0, "3d3": 0}
seeded = notseeded = noline = 0
bits, jbits, batches, ms, alone, secs = [], [], [], [], [], []
src = {}
unseeded_3d8 = 0
fail_lines = []
for d in dirs:
    for l in open(os.path.join(d, "summary.txt")):
        m = re.match(r"boot=(\d+) end=(\w+) secs=([\d.]+)", l)
        if not m:
            continue
        boots += 1
        ends += m.group(2) == "marker"
        secs.append(float(m.group(3)))
        t = open(os.path.join(d, f"lines-{m.group(1)}.txt"), encoding="utf-8", errors="replace").read()
        for tag in ok:
            ls = [x for x in t.splitlines() if x.startswith(f"[{tag}]")]
            if not ls:
                missing[tag] += 1
            elif any("FAILED" in x for x in ls):
                failed[tag] += 1
                fail_lines.append(f"{d} boot {m.group(1)}: {ls[0][:300]}")
            else:
                ok[tag] += 1
        a = re.search(r"jitter-alone≈(\d+)bits", t)
        if a:
            alone.append(int(a.group(1)))
        if re.search(r"seeded\([^)]*\)-ready=false", t):
            unseeded_3d8 += 1
        e = [x for x in t.splitlines() if x.startswith("[entropy] CSPRNG")]
        if not e:
            noline += 1
            continue
        e = e[0]
        if "NOT SEEDED" in e:
            notseeded += 1
        else:
            seeded += 1
        m2 = re.search(r"(\d+) of (\d+) bits .*tpm=([^,]+), rdseed=([^,]+), rdrand=(.+?) \(CPU credit (\d+) bits.*jitter=(\d+) batches of 512 samples \((\d+) bits\) · ([\d.]+) ms", e)
        if m2:
            bits.append(int(m2.group(1))); batches.append(int(m2.group(7))); jbits.append(int(m2.group(8))); ms.append(float(m2.group(9)))
            k = f"tpm={m2.group(3)}, rdseed={m2.group(4)}, rdrand={m2.group(5)}, CPU credit {m2.group(6)}"
            src[k] = src.get(k, 0) + 1

def rng(v, fmt="{:g}"):
    return "-" if not v else f"{fmt.format(min(v))} / {fmt.format(st.median(v))} / {fmt.format(max(v))}"

print(f"== {title}")
print(f"boots: {boots} ({ends} reached the marker); seconds per boot min/median/max: {rng(secs, '{:.1f}')}")
for tag in ("3d8", "3d7", "3d3"):
    print(f"[{tag}]: OK in {ok[tag]}, FAILED in {failed[tag]}, no line in {missing[tag]}")
print(f"[3d8] reports the global pool not ready (unseeded boot): {unseeded_3d8}")
if seeded + notseeded + noline and (seeded or notseeded):
    print(f"[entropy] line: seeded in {seeded}, NOT SEEDED in {notseeded}, no line in {noline}")
    print(f"  credited bits min/median/max: {rng(bits)}; of which jitter: {rng(jbits)}; jitter batches: {rng(batches)}")
    print(f"  time of entropy::init, ms, min/median/max: {rng(ms, '{:.3f}')}")
    for k, n in sorted(src.items(), key=lambda kv: -kv[1]):
        print(f"  sources in {n} boots: {k}")
else:
    print("[entropy] line: this image has none")
if alone:
    print(f"[3d8] jitter-alone estimate over 16 batches, bits, min/median/max: {rng(alone)}; below 256 in {sum(a < 256 for a in alone)} boots")
for l in fail_lines[:20]:
    print("  FAILED: " + l)
