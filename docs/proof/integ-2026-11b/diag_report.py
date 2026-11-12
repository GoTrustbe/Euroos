#!/usr/bin/env python3
"""bug065-diagnosis.txt from the [ent-diag] lines of the diagnostic image (entdiag.rs.txt).
usage: diag_report.py IDLE_DIR LOAD_DIR"""
import glob, re, statistics as st, sys

def load(d):
    rows = []
    for f in sorted(glob.glob(d + "/*lines-*.txt")):
        t = open(f, errors="replace").read()
        tot = re.search(r"old64=(\d+) tsc=(\d+) per=\[([^]]*)\] \| asm-aligned64=(\d+) asm-straddle64=(\d+) tsc=\d+ \| mem64=(\d+) tsc=(\d+)", t)
        if not tot:
            continue
        r = {"old64": int(tot[1]), "tsc_old": int(tot[2]), "per": [int(x) for x in tot[3].split(",")],
             "a64": int(tot[4]), "b64": int(tot[5]), "mem64": int(tot[6]), "tsc_mem": int(tot[7])}
        for tag in ("old r0", "old r1", "old r2", "asm-aligned", "asm-straddle", "mem r0", "mem r1"):
            m = re.search(re.escape(tag) + r" bits=(\d+) min=(\d+) med=(\d+) max=(\d+) distinct=(\d+) mode=(\d+)x(\d+) first64=\[([^]]*)\]", t)
            if m:
                r[tag] = {"bits": int(m[1]), "min": int(m[2]), "med": int(m[3]), "max": int(m[4]), "distinct": int(m[5]),
                          "mode": int(m[6]), "modecnt": int(m[7]), "first": [int(x) for x in m[8].split(",")]}
        rows.append(r)
    return rows

def f3(v):
    return f"{min(v)} / {int(st.median(v))} / {max(v)}"

def period(s):
    """Smallest p <= 16 for which at least 90 % of the samples equal the one p places before."""
    for p in range(1, 17):
        same = sum(1 for i in range(p, len(s)) if s[i] == s[i - p])
        if same >= 0.9 * (len(s) - p):
            return p
    return 0

print("# BUG-065, diagnosis. Diagnostic kernel: the merged tree 1c8eb91 plus kernel/src/entdiag.rs (entdiag.rs.txt in this")
print("# directory; never committed to the kernel), called before entropy::selftest. R630, KVM, -cpu host, smp 4, 2 GiB.")
print("# 'old' is the two-RDTSC loop of entropy.rs as it was; 'asm aligned' and 'asm straddling' are the same loop in")
print("# assembly, once inside one 64-byte line and once across a line boundary; 'memory walk' is the new sample.")
print("# Credit is estimate_jitter_bits over batches of 512 samples. Three numbers are min / median / max over the boots.")
for name, d in (("host idle", sys.argv[1]), ("all 40 hyperthreads of the host busy (nice 19 loops)", sys.argv[2])):
    rows = load(d)
    print(f"\n== {name}: {len(rows)} boots")
    print(f"credit over 64 batches   old loop: {f3([r['old64'] for r in rows])}   asm aligned: {f3([r['a64'] for r in rows])}   asm straddling: {f3([r['b64'] for r in rows])}   memory walk: {f3([r['mem64'] for r in rows])}")
    more = sum(r['b64'] > r['a64'] for r in rows); less = sum(r['b64'] < r['a64'] for r in rows)
    print(f"placement: the straddling copy was credited more than the aligned one in {more} boots, less in {less}")
    allper = [p for r in rows for p in r["per"]]
    print(f"old loop, credit per batch over all {len(allper)} batches: {f3(allper)}; batches under 32 bits: {sum(p < 32 for p in allper)}")
    print(f"TSC ticks for 64 batches (median): old loop {int(st.median(r['tsc_old'] for r in rows))}, memory walk {int(st.median(r['tsc_mem'] for r in rows))}")
    for tag, label in (("old r0", "old loop, first batch"), ("mem r0", "memory walk, first batch")):
        v = [r[tag] for r in rows if tag in r]
        print(f"{label}: distinct sample values per batch {f3([x['distinct'] for x in v])}; share of the most frequent value {min(x['modecnt'] for x in v) * 100 // 512} to {max(x['modecnt'] for x in v) * 100 // 512} %; sample min {min(x['min'] for x in v)}, median of medians {int(st.median(x['med'] for x in v))}; "
              f"boots whose first 64 samples repeat with a period of at most 16: {sum(1 for x in v if period(x['first']))} of {len(v)}")
    kinds = {}
    for r in rows:
        if "old r0" in r:
            k = (r["old r0"]["min"], r["old r0"]["max"], r["old r0"]["distinct"])
            kinds.setdefault(k, []).append(r)
    print("old loop, first batch, by (min, max, distinct values): boots, credit for that batch, first 24 samples of one boot")
    for k, rs in sorted(kinds.items(), key=lambda kv: -len(kv[1]))[:6]:
        print(f"  {k}: {len(rs)} boots, {min(r['old r0']['bits'] for r in rs)} to {max(r['old r0']['bits'] for r in rs)} bits, e.g. {rs[0]['old r0']['first'][:24]}")
    r = rows[0]
    print(f"memory walk, first 24 samples of one boot: {r['mem r0']['first'][:24]}")
