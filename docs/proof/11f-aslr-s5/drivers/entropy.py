#!/usr/bin/env python3
"""entropy.py: varying bits per region over boots (first [aslr] start of each boot) and over
Chromium launches/execs ([aslr-layout] lines). usage: entropy.py aslr LOG... | entropy.py chrome LOG..."""
import re, sys
def varying(xs):
    a = 0
    for x in xs: a |= x ^ xs[0]
    return bin(a).count("1")
mode, logs = sys.argv[1], sys.argv[2:]
if mode == "aslr":
    keys = ["exe", "ldso", "libc", "stack", "brk", "mmap", "mmapbig", "vdso"]
    firsts = {k: [] for k in keys}; per = {k: [] for k in keys}; verdicts = []
    for l in logs:
        t = open(l, "rb").read().decode("utf-8", "replace")
        for k in keys:
            m = re.search(r"^\[aslr\]\s+%s\s+first (0x[0-9a-f]+)\s+varying bits\s+(\d+)" % k, t, re.M)
            if m: firsts[k].append(int(m.group(1), 16)); per[k].append(int(m.group(2)))
        m = re.search(r"^\[aslr\] \d+ starts.*", t, re.M)
        verdicts.append("OK" if m and "OK" in m.group(0) else ("FAILED" if m else "none"))
    print("boots %d; [aslr] verdicts: %s" % (len(logs), ", ".join("%s %d" % (v, verdicts.count(v)) for v in sorted(set(verdicts)))))
    print("%-8s %10s %22s  %s" % ("region", "over boots", "per boot min..max", "distinct firsts"))
    for k in keys:
        xs = firsts[k]
        if xs: print("%-8s %10d %18d..%-3d  %d/%d" % (k, varying(xs), min(per[k]), max(per[k]), len(set(xs)), len(xs)))
else:
    rx = re.compile(r"^\[aslr-layout\] (launch|exec) .*")
    launch, execs = [], []
    for l in logs:
        for line in open(l, "rb").read().decode("utf-8", "replace").splitlines():
            if not rx.match(line): continue
            d = dict((k, int(v, 16)) for k, v in re.findall(r"\b(exe|floor|vdso|arena|ld\.so|brk|stack) (0x[0-9a-f]+)", line))
            (launch if " launch " in line else execs).append(d)
    for name, rows in (("launch", launch), ("exec", execs)):
        if not rows: print("%s: no lines" % name); continue
        out = []
        for k in ("exe", "ld.so", "arena", "brk", "stack", "floor", "vdso"):
            xs = [r[k] for r in rows if k in r and r[k]]
            if xs: out.append("%s %d" % (k, varying(xs)))
        print("%s: %d lines; varying bits: %s" % (name, len(rows), ", ".join(out)))
