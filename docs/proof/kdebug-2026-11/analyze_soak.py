#!/usr/bin/env python3
"""Summary of a loadtest.py soak: commands, heap slope after 10 min (least squares),
counters first/last, glibc launches, fault lines. usage: analyze_soak.py WORKDIR OUT.txt"""
import csv, glob, os, re, sys

w, out = sys.argv[1], sys.argv[2]
rows = list(csv.DictReader(open(os.path.join(w, "loadtest-results-ltstat.csv"))))
t = [float(r["t_s"]) for r in rows]
heap = [int(r["heap_used"]) for r in rows]
print(f"ltstat samples: {len(rows)}, t = {t[0]:.0f} .. {t[-1]:.0f} s ({t[-1] / 60:.1f} min)")
i10 = next(i for i, v in enumerate(t) if v >= 600)
xs, ys = t[i10:], heap[i10:]
n = len(xs)
mx, my = sum(xs) / n, sum(ys) / n
slope = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sum((x - mx) ** 2 for x in xs)
print(f"heap_used first / at 10 min / last: {heap[0]:,} / {heap[i10]:,} / {heap[-1]:,}".replace(",", " "))
print(f"heap slope after 10 min (least squares): {slope * 60 / 1000:+.2f} kB/min, range {(max(ys) - min(ys)) / 1000:.0f} kB")
for k in ("tasks", "files", "kstacks", "procpool_free", "frames_free", "bg_zombie", "bg_live"):
    v = [int(r[k]) for r in rows]
    print(f"{k}: first {v[0]}, at 10 min {v[i10]}, last {v[-1]}, min {min(v[i10:])}, max {max(v[i10:])} (after 10 min)")
res = list(csv.DictReader(open(os.path.join(w, "loadtest-results.csv"))))
cols = res[0].keys() if res else []
print(f"commands: {len(res)}; columns: {', '.join(cols)}")
stat = next((c for c in cols if c.lower() in ("status", "result", "ok", "verdict")), None)
if stat:
    from collections import Counter
    print("by status:", dict(Counter(r[stat] for r in res)))
cmdcol = next((c for c in cols if c.lower() in ("cmd", "command")), None)
if cmdcol:
    print("churn proc commands:", sum(1 for r in res if r[cmdcol].startswith("churn proc")))
ser = glob.glob(os.path.join(w, "loadtest-serial-*.log"))[0]
txt = open(ser, errors="replace").read()
print("serial:", os.path.basename(ser), f"{len(txt) / 1e6:.1f} MB")
for pat in ("GLIBC-DYNAMIC-OK", "KERNEL PANIC", "GENERAL PROTECTION", "[idt] PAGE FAULT", "DOUBLE FAULT", "NMI PROBE",
            "[watchdog]", "refused to free", "out of memory: page tables", "no frames for page tables"):
    print(f"  {pat!r}: {txt.count(pat)}")
cp = re.findall(r"^\[churn-proc\].*$", txt, flags=re.M)
print(f"  [churn-proc] verdict lines: {len(cp)}, with FAILED: {sum('FAILED' in l for l in cp)}")
lk = [l for l in txt.splitlines() if l.startswith("[lk] boot-total order")]
print("  " + (lk[-1] if lk else "no [lk] boot-total line"))
o = open(out, errors="replace").read().splitlines()
print("--- end of the harness output:")
for l in o[-25:]:
    print(l)
