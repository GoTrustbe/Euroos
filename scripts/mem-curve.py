#!/usr/bin/env python3
"""Memory by owner over a browser session, from the serial log of a chrome-smoke run.

The kernel prints `[mem]` and `[memstat]` at every heartbeat (30 s) and whenever the
shell command `ramfiles` or `memstat` runs; chrome-smoke asks for `ramfiles` after every
site of its matrix. This script reads both:

  mem-curve.py serial.log [harness-output.txt] [--csv curve.csv] [--warmup-min 15]

  * one row per site step (when the harness output is given): the census right after
    the site, and what the site added to each owner since the step before;
  * the time series of the heartbeats (--csv);
  * the slope of every owner after the warm-up, in MiB per hour (least squares), and
    the verdict "flat" when what the pool has in use grows by less than 5% of the pool
    per hour and no kill line is in the log.

Every figure is MiB as the kernel printed it. No guest is started here.
"""
import argparse, re, sys

SUM = re.compile(r"\[memstat\] sum tick=(\d+) processes=(\d+) anon=(\d+) \(zero~(\d+) no-access=(\d+)\) file-private=(\d+) "
                 r"tables=(\d+) pagecache=(\d+) shm=(\d+) free=(\d+) pool=(\d+) unattributed=(-?\d+) KiB")
MEM = re.compile(r"\[mem\] tick=(\d+) pool=(\d+) free=(\d+) pagecache=(\d+) shm=(\d+) \(named (\d+), (\d+) files\) processes=(\d+) "
                 r"\| kernel heap used=(\d+) free=(\d+) big=(\d+)/(\d+) ramfile-bytes=(\d+) in (\d+) files \| process pool=(\d+) free=(\d+)")
PROC = re.compile(r"\[memstat\] pid=(\d+) task=(\d+) name=\"([^\"]*)\" anon=(\d+) \(zero~(\d+) no-access=(\d+)\) file-private=(\d+) tables=(\d+)")
KILL = re.compile(r"\[isolation\].*TERMINATED|\[demand\] POOL EXHAUSTED|\[oom\] .*killed|KERNEL PANIC|panicked at")
SITE = re.compile(r"\[gui-smoke\] (ok  |FAIL) site ([\w-]+):")
KEYS = ["anon", "zero", "noaccess", "fileprivate", "tables", "pagecache", "shm", "free", "pool", "unattributed_kib",
        "heap_used", "big_used", "ramfile_bytes", "procpool_free", "processes"]


def parse(path):
    """-> (samples, kills). A sample: dict of KEYS plus tick, at_ramfiles (bool), procs."""
    samples, kills, cur, procs, after_ramfiles = [], [], None, [], False
    with open(path, "rb") as f:
        for raw in f:
            line = raw.decode("utf-8", "replace")
            if "[ramfiles] ramfiles:" in line:
                after_ramfiles = True
            m = MEM.search(line)
            if m:
                g = [int(x) for x in m.groups()]
                cur = dict(tick=g[0], heap_used=g[8], big_used=g[10], ramfile_bytes=g[12], procpool_free=g[15],
                           shm_named=g[5], shm_files=g[6])
                procs = []
                continue
            m = PROC.search(line)
            if m and cur is not None:
                procs.append((int(m.group(1)), m.group(3), int(m.group(4)), int(m.group(5)), int(m.group(6))))
                continue
            m = SUM.search(line)
            if m and cur is not None:
                g = [int(x) for x in m.groups()]
                cur.update(processes=g[1], anon=g[2], zero=g[3], noaccess=g[4], fileprivate=g[5], tables=g[6],
                           pagecache=g[7], shm=g[8], free=g[9], pool=g[10], unattributed_kib=g[11],
                           at_ramfiles=after_ramfiles, procs=procs)
                samples.append(cur)
                cur, after_ramfiles = None, False
                continue
            if KILL.search(line):
                kills.append((samples[-1]["tick"] if samples else 0, line.strip()[:200]))
    return samples, kills


def slope(xs, ys):
    """Least-squares slope of ys over xs."""
    n = len(xs)
    if n < 2:
        return 0.0
    mx, my = sum(xs) / n, sum(ys) / n
    den = sum((x - mx) ** 2 for x in xs)
    return sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / den if den else 0.0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("serial")
    ap.add_argument("harness", nargs="?")
    ap.add_argument("--csv")
    ap.add_argument("--warmup-min", type=float, default=15.0, help="minutes of the session not counted for the slope")
    ap.add_argument("--hz", type=float, default=100.0, help="kernel ticks per second")
    a = ap.parse_args()
    samples, kills = parse(a.serial)
    if not samples:
        sys.exit("no [memstat] sum line in the log")
    t0 = samples[0]["tick"]
    mins = lambda s: (s["tick"] - t0) / a.hz / 60.0
    print(f"{len(samples)} census samples over {mins(samples[-1]):.0f} min; pool {samples[0]['pool']} MiB")

    if a.harness:
        steps = [(m.group(1).strip(), m.group(2)) for m in SITE.finditer(open(a.harness, errors="replace").read())]
        at = [s for s in samples if s["at_ramfiles"]]
        print("\nper site step (MiB; d = change of what the pool has in use since the step before)")
        print(f"{'min':>5} {'rnd':>3} {'site':<14} {'ok':<4} {'used':>5} {'d':>5} {'anon':>5} {'zero~':>5} {'noacc':>5} {'filep':>5} "
              f"{'tabl':>4} {'pcache':>6} {'shm':>4} {'free':>5} {'heap':>4} {'ramf':>4} {'procs':>5}")
        prev, rnd, seen = None, 1, set()
        per_site = {}
        for (ok, name), s in zip(steps, at):
            if name in seen:
                rnd, seen = rnd + 1, set()
            seen.add(name)
            used = s["pool"] - s["free"]
            d = used - prev if prev is not None else 0
            prev = used
            per_site.setdefault(name, []).append(d)
            print(f"{mins(s):5.0f} {rnd:3d} {name:<14} {ok:<4} {used:5d} {d:+5d} {s['anon']:5d} {s['zero']:5d} {s['noaccess']:5d} "
                  f"{s['fileprivate']:5d} {s['tables']:4d} {s['pagecache']:6d} {s['shm']:4d} {s['free']:5d} {s['heap_used']:4d} "
                  f"{s['ramfile_bytes']:4d} {s['processes']:5d}")
        print("\nwhat a site adds to the memory in use, per round (MiB): first round, then the later ones")
        for name, ds in per_site.items():
            later = ds[1:]
            print(f"  {name:<14} first {ds[0]:+5d}   later {' '.join(f'{d:+d}' for d in later[:12])}"
                  + (f"   mean {sum(later) / len(later):+.0f}" if later else ""))

    if a.csv:
        with open(a.csv, "w") as f:
            f.write("minute," + ",".join(KEYS) + "\n")
            for s in samples:
                f.write(f"{mins(s):.2f}," + ",".join(str(s[k]) for k in KEYS) + "\n")

    late = [s for s in samples if mins(s) >= a.warmup_min]
    print(f"\nafter the warm-up ({a.warmup_min:.0f} min): {len(late)} samples over {mins(samples[-1]) - a.warmup_min:.0f} min")
    if len(late) >= 4:
        xs = [mins(s) / 60.0 for s in late]
        print(f"{'owner':<22} {'first':>6} {'last':>6} {'min':>6} {'max':>6} {'MiB/h':>8}")
        rows = [("in use (pool - free)", [s["pool"] - s["free"] for s in late])]
        rows += [(k, [s[k] for s in late]) for k in ("anon", "zero", "noaccess", "fileprivate", "tables", "pagecache", "shm",
                                                    "heap_used", "big_used", "ramfile_bytes")]
        for name, ys in rows:
            print(f"{name:<22} {ys[0]:6d} {ys[-1]:6d} {min(ys):6d} {max(ys):6d} {slope(xs, ys):+8.1f}")
        used = rows[0][1]
        sl = slope(xs, used)
        pool = late[-1]["pool"]
        flat = sl < 0.05 * pool and not kills
        print(f"\nlowest free: {min(s['free'] for s in samples)} MiB of {pool}; largest unattributed: "
              f"{max(abs(s['unattributed_kib']) for s in samples)} KiB")
        print(f"verdict: {'flat' if flat else 'NOT flat'} (in use {sl:+.1f} MiB/h = {100 * sl / pool:+.1f}% of the pool per hour; "
              f"{len(kills)} kill/exhaustion lines)")
    for tick, line in kills[:12]:
        print(f"  kill line near minute {(tick - t0) / a.hz / 60:.0f}: {line}")
    last = samples[-1]
    print("\nlast census, per process (pid name anon zero~ no-access):")
    for pid, name, anon, zero, na in last["procs"]:
        print(f"  {pid:5d} {name:<18} {anon:5d} {zero:5d} {na:5d}")


if __name__ == "__main__":
    main()
