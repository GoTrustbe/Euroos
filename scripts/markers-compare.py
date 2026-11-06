#!/usr/bin/env python3
"""Per-marker verdicts of EuroOS serial logs, and a comparison.

usage: scripts/markers-compare.py summary LOG
       scripts/markers-compare.py compare BASE NEW [--only PREFIX,...]
A marker line starts with "[tag]". Verdict: FAIL if it says FAILED, OK if it
has a check mark or "-> OK", SKIP if it has the open box, else not a verdict.
"""
import re
import sys
from collections import Counter, defaultdict

TAG = re.compile(r"^\[([a-z0-9][a-z0-9_./-]*)\]")


def verdicts(path):
    out = defaultdict(Counter)
    with open(path, "rb") as f:
        for raw in f:
            line = raw.decode("utf-8", "replace").rstrip("\n")
            m = TAG.match(line)
            if not m:
                continue
            tag = m.group(1)
            if "FAILED" in line:
                v = "FAIL"
            elif "✓" in line or "→ OK" in line:
                v = "OK"
            elif "⬜" in line:
                v = "SKIP"
            else:
                continue
            out[tag][v] += 1
    return out


def fmt(c):
    return " ".join(f"{k}x{c[k]}" for k in ("OK", "FAIL", "SKIP") if c.get(k)) or "-"


def main():
    mode = sys.argv[1]
    if mode == "summary":
        v = verdicts(sys.argv[2])
        ok = sum(c["OK"] for c in v.values())
        fail = {t: c["FAIL"] for t, c in v.items() if c["FAIL"]}
        print(f"{sys.argv[2]}: {len(v)} tags, {ok} OK lines, FAILED: {fail or 'none'}")
        return
    base, new = verdicts(sys.argv[2]), verdicts(sys.argv[3])
    only = None
    if "--only" in sys.argv:
        only = sys.argv[sys.argv.index("--only") + 1].split(",")
    tags = sorted(set(base) | set(new))
    worse, better, gone, added = [], [], [], []
    for t in tags:
        if only and not any(t.startswith(p) for p in only):
            continue
        b, n = base.get(t), new.get(t)
        if b and not n:
            gone.append(f"{t}: {fmt(b)}")
        elif n and not b:
            added.append(f"{t}: {fmt(n)}")
        elif set(b) != set(n) or (b["FAIL"] != n["FAIL"]):
            line = f"{t}: base {fmt(b)} | new {fmt(n)}"
            (worse if n["FAIL"] > b["FAIL"] or (b["OK"] and not n["OK"]) else better).append(line)
    print(f"== {sys.argv[2]}  vs  {sys.argv[3]}")
    for title, lst in (("WORSE", worse), ("CHANGED (not worse)", better), ("only in base", gone), ("only in new", added)):
        print(f"-- {title}: {len(lst)}")
        for l in lst:
            print("   " + l)


if __name__ == "__main__":
    main()
