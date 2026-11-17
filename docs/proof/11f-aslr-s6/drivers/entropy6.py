#!/usr/bin/env python3
"""entropy6.py: varying bits per region and per distance.
  entropy6.py aslr LOG...    over boots: the first [aslr] start of each boot; per boot: the [aslr] and [aslr-ind] figures
  entropy6.py chrome LOG...  over Chromium launches / exec'd children ([aslr-layout] lines)
'varying bits' = bits that differ from the first value in at least one value (the statistic of [aslr])."""
import re, sys
def varying(xs):
    a = 0
    for x in xs: a |= x ^ xs[0]
    return bin(a).count("1")
def dist(rows, a, b):
    xs = [(r[a] - r[b]) & (2**64 - 1) for r in rows if r.get(a) and r.get(b)]
    return varying(xs) if len(xs) > 1 else None
mode, logs = sys.argv[1], sys.argv[2:]
if mode == "aslr":
    keys = ["exe", "ldso", "libc", "stack", "brk", "mmap", "mmapbig", "vdso"]
    rows, per, verdicts, ind = [], {k: [] for k in keys}, [], []
    for l in logs:
        t = open(l, "rb").read().decode("utf-8", "replace")
        r = {}
        for k in keys:
            m = re.search(r"^\[aslr\]\s+%s\s+first (0x[0-9a-f]+)\s+varying bits\s+(\d+)" % k, t, re.M)
            if m: r[k] = int(m.group(1), 16); per[k].append(int(m.group(2)))
        if r: rows.append(r)
        m = re.search(r"^\[aslr\] \d+ starts.*", t, re.M)
        verdicts.append("OK" if m and "OK" in m.group(0) else ("FAILED" if m else "none"))
        m = re.search(r"^\[aslr-ind\] .*", t, re.M)
        if m: ind.append(m.group(0))
    print("boots %d; [aslr] verdicts: %s" % (len(logs), ", ".join("%s %d" % (v, verdicts.count(v)) for v in sorted(set(verdicts)))))
    print("%-8s %10s %18s  %s" % ("region", "over boots", "per boot min..max", "distinct firsts"))
    for k in keys:
        xs = [r[k] for r in rows if k in r]
        if xs: print("%-8s %10d %14d..%-3d  %d/%d" % (k, varying(xs), min(per[k]), max(per[k]), len(set(xs)), len(xs)))
    print("distance over boots (first start of each boot):")
    for n, a, b in [("exe-heap (brk)", "brk", "exe"), ("heap-stack (brk-stack)", "stack", "brk"), ("exe-stack", "stack", "exe"), ("exe-mmap 4 KiB", "mmap", "exe"), ("exe-mmap 1 MiB", "mmapbig", "exe"), ("exe-vDSO", "vdso", "exe"), ("stack-mmap 4 KiB", "stack", "mmap"), ("stack-ld.so", "stack", "ldso"), ("ld.so-exe (same segment)", "ldso", "exe"), ("mmap-brk (same segment)", "mmap", "brk")]:
        print("  %-26s %s" % (n, dist(rows, a, b)))
    pairs = {}
    for line in ind:
        for name, v in re.findall(r"([a-zA-Z0-9.]+-[a-zA-Z0-9.]+) (\d+)", line):
            pairs.setdefault(name, []).append(int(v))
    verd = [("OK" if "OK" in l else "FAILED" if "FAILED" in l else "other") for l in ind]
    print("[aslr-ind] per boot: %s" % ", ".join("%s %d" % (v, verd.count(v)) for v in sorted(set(verd))))
    for name, vs in pairs.items():
        print("  %-18s min %d max %d" % (name, min(vs), max(vs)))
else:
    rx = re.compile(r"^\[aslr-layout\] (launch|exec) .*")
    launch, execs = [], []
    for l in logs:
        for line in open(l, "rb").read().decode("utf-8", "replace").splitlines():
            if not rx.match(line): continue
            d = dict((k, int(v, 16)) for k, v in re.findall(r"\b(exe|floor|vdso|arena|hseg|sseg|ld\.so|brk|stack) (0x[0-9a-f]+)", line))
            (launch if " launch " in line else execs).append(d)
    for name, rows in (("launch", launch), ("exec", execs)):
        if not rows: print("%s: no lines" % name); continue
        out = []
        for k in ("exe", "ld.so", "arena", "hseg", "sseg", "brk", "stack", "floor", "vdso"):
            xs = [r[k] for r in rows if k in r and r[k]]
            if len(xs) > 1: out.append("%s %d" % (k, varying(xs)))
        print("%s: %d lines; varying bits: %s" % (name, len(rows), ", ".join(out)))
        ds = []
        for n, a, b in [("ld.so-brk", "brk", "ld.so"), ("brk-stack", "stack", "brk"), ("ld.so-stack", "stack", "ld.so"), ("exe-brk", "brk", "exe"), ("exe-stack", "stack", "exe"), ("exe-floor", "floor", "exe"), ("exe-vdso", "vdso", "exe"), ("stack-floor", "floor", "stack")]:
            v = dist(rows, a, b)
            if v is not None: ds.append("%s %d" % (n, v))
        print("%s: distances varying bits: %s" % (name, ", ".join(ds)))
