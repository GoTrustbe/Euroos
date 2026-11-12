#!/usr/bin/env python3
"""Compare two soak result files of scripts/loadtest.py (columns id, command,
status, elapsed_ms, output_lines, t_s): commands per hour, and what the
commands that start processes cost. usage: soak_compare.py NAME=CSV [NAME=CSV ...] [--window S]

--window S: only commands with t_s <= S (to compare the same stretch of two
soaks of different length)."""
import csv
import statistics
import sys


def load(path, window):
    rows = []
    with open(path, newline="") as f:
        for r in csv.DictReader(f):
            try:
                t, ms = float(r["t_s"]), float(r["elapsed_ms"])
            except (KeyError, ValueError):
                continue
            if window and t > window:
                continue
            rows.append((r["command"], r["status"], ms, t))
    return rows


def describe(name, rows):
    if not rows:
        print(f"{name}: no rows")
        return
    span = rows[-1][3] - rows[0][3]
    by = {}
    for c, s, ms, t in rows:
        by[s] = by.get(s, 0) + 1
    print(f"{name}: {len(rows)} commands over {span / 60:.1f} min = {len(rows) / span * 3600:.0f} per hour; by status {by}")
    for prefix in ("churn proc", "spawn worker", "churn fs", "churn net", "sha256sum", "fetch"):
        ms = [r[2] for r in rows if r[0].startswith(prefix)]
        if ms:
            ms.sort()
            print(f"   {prefix:<13} n={len(ms):4d}  median {statistics.median(ms):8.0f} ms  mean {statistics.mean(ms):8.0f} ms  "
                  f"p10 {ms[len(ms) // 10]:8.0f}  p90 {ms[len(ms) * 9 // 10]:8.0f}  max {ms[-1]:8.0f}")


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    window = 0.0
    if "--window" in sys.argv:
        window = float(sys.argv[sys.argv.index("--window") + 1])
        args = [a for a in args if a != sys.argv[sys.argv.index("--window") + 1]]
    for a in args:
        name, path = a.split("=", 1)
        describe(name, load(path, window))


if __name__ == "__main__":
    main()
