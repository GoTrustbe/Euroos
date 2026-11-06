#!/usr/bin/env python3
"""Name the allocation sites of a heaptrace image (BUG-027).

usage: scripts/heaptrace-sym.py target/kernel.map SERIAL_LOG [--all] [--last]

Reads the `[heaptrace] ... anchor=0x...` header and the `[heaptrace] grew|live
site=...` lines that `heapdiff` / `heaptop` print (kernel/src/heaptrace.rs),
turns each return address into function+offset with the lld link map (the
same map scripts/symbolize.sh uses), and prints per site the first frame
outside the allocator and the generic collection code, then the chain.
--last: only the last report in the log (default: every report).
"""
import bisect
import re
import sys

IMGBASE = 0x140000000
MAPLINE = re.compile(r"^\s*[0-9a-fA-F]{4}:[0-9a-fA-F]{8}\s+(\S+)\s+([0-9a-fA-F]{16})\s")
HDR = re.compile(r"^\[heaptrace\] (diff since heapsnap|top live) anchor=(0x[0-9a-f]+) sum=([+-]?\d+)")
SITE = re.compile(r"^\[heaptrace\] (grew|live) site=(\d+) bytes=([+-]?\d+) blocks=([+-]?\d+) live=(-?\d+) total=(\d+) frames=(\S*)")

# Frames that only say "something allocated": skipped when naming a site.
BORING = ("alloc::", "core::", "__rust", "eurokernel::allocator", "eurokernel::heaptrace",
          "hashbrown::", "<alloc::", "<core::")


def demangle(sym):
    """Readable path of a Rust v0 (`_R...`) or legacy (`_ZN...E`) symbol; a
    heuristic (identifiers in order, disambiguators dropped), enough to read."""
    if sym.startswith("_ZN"):
        out, i, s = [], 3, sym
        while i < len(s) and s[i].isdigit():
            j = i
            while s[j].isdigit():
                j += 1
            n = int(s[i:j])
            ident = s[j:j + n]
            i = j + n
            if not re.fullmatch(r"h[0-9a-f]{16}", ident):
                out.append(ident)
        return "::".join(out).replace("$LT$", "<").replace("$GT$", ">").replace("$u20$", " ").replace("..", "::")
    if not sym.startswith("_R"):
        return sym
    s = sym[2:]
    s = re.sub(r"C(s[0-9A-Za-z]*_)", "C", s)          # crate disambiguators
    s = re.sub(r"(N[A-Za-z])s[0-9A-Za-z]*_", r"\1", s)  # nested-path disambiguators
    s = re.sub(r"B[0-9A-Za-z]*_", "", s)                 # back references
    out, i = [], 0
    while i < len(s):
        if s[i].isdigit():
            j = i
            while j < len(s) and s[j].isdigit():
                j += 1
            n = int(s[i:j])
            if j < len(s) and s[j] == "_":
                j += 1
            out.append(s[j:j + n])
            i = j + n
        else:
            i += 1
    return "::".join(x for x in out if x)


def load_map(path):
    syms = []
    with open(path, errors="replace") as f:
        for line in f:
            m = MAPLINE.match(line)
            if m:
                syms.append((int(m.group(2), 16) - IMGBASE, m.group(1)))
    syms.sort()
    return [a for a, _ in syms], [n for _, n in syms]


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(2)
    addrs, names = load_map(sys.argv[1])
    anchor_rva = next(a for a, n in zip(addrs, names) if n.endswith("dump_registers_and_backtrace"))
    reports, cur = [], None
    with open(sys.argv[2], "rb") as f:
        for raw in f:
            line = raw.decode("utf-8", "replace").strip()
            m = HDR.match(line)
            if m:
                cur = {"kind": m.group(1), "base": int(m.group(2), 16) - anchor_rva, "sum": int(m.group(3)), "sites": []}
                reports.append(cur)
                continue
            m = SITE.match(line)
            if m and cur is not None:
                frames = [int(x, 16) for x in m.group(7).split(",") if x]
                cur["sites"].append((int(m.group(2)), int(m.group(3)), int(m.group(4)), int(m.group(6)), frames))
    if "--last" in sys.argv:
        reports = reports[-1:]

    def name(rt, base):
        rva = rt - base
        k = bisect.bisect_right(addrs, rva) - 1
        if k < 0:
            return f"?{rt:#x}"
        return f"{demangle(names[k])}+{rva - addrs[k]:#x}"

    def func(n):
        return n.rsplit("+", 1)[0]

    for r in reports:
        print(f"== {r['kind']}: sum {r['sum']:+} bytes over all sites")
        groups = {}
        for site, b, c, total, frames in r["sites"]:
            chain = [name(x, r["base"]) for x in frames]
            interesting = [n for n in chain if not n.startswith(BORING)]
            first = interesting[0] if interesting else (chain[0] if chain else "?")
            # Group by the first two functions outside the allocator: one
            # call site whose deeper frames differ is still one site.
            key = " <- ".join(func(n) for n in interesting[:2]) or first
            g = groups.setdefault(key, [0, 0, 0, []])
            g[0] += b
            g[1] += c
            g[2] += total
            g[3].append(site)
            print(f"{b:+12} B {c:+8} blocks  (allocs ever {total})  site {site}: {first}")
            if "--all" in sys.argv:
                for n in chain:
                    print(f"{'':28}{n}")
        print(f"-- grouped by the first two frames outside the allocator")
        for key, (b, c, total, sites) in sorted(groups.items(), key=lambda kv: -kv[1][0]):
            print(f"{b:+12} B {c:+8} blocks  (allocs ever {total}, {len(sites)} site(s))  {key}")


if __name__ == "__main__":
    main()
