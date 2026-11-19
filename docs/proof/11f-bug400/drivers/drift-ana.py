#!/usr/bin/env python3
"""drift-ana.py SAMPLES.txt...: the drift of a vdrift.py run from its samples file.
The first `uptime` after the console comes up answers 70-90 ms late (the console
is still busy after boot), which is 40-50 ppm over 30 min; so the first sample is
left out and the rate is the least-squares slope over the others (and the plain
second-to-last difference beside it). Rates against the host CLOCK_MONOTONIC (the
host time at the arrival of each line): MONOTONIC, REALTIME (vs host wall), and from
the diag line the HPET, TSC, tick count, timer interrupts, ticks the interrupts added."""
import sys

def slope(xs, ys):
    n = len(xs); mx = sum(xs) / n; my = sum(ys) / n
    return sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sum((x - mx) ** 2 for x in xs)

for f in sys.argv[1:]:
    s, d = [], []
    for l in open(f):
        p = l.split()
        if p[0] == "diag":
            d.append([float(p[1])] + [int(x) for x in p[2:]])
        else:
            s.append((float(p[0]), float(p[1]), int(p[2]), int(p[3])))
    s, d = s[1:], d[1:]
    if len(s) < 3:
        print(f, "too few samples"); continue
    hm = [x[0] for x in s]; hr = [x[1] for x in s]
    mono = slope(hm, [x[2] / 1e9 for x in s]); real = slope(hr, [x[3] / 1e9 for x in s])
    span = hm[-1] - hm[0]
    off = max(abs((x[2] - s[0][2]) / 1e9 - mono * (x[0] - s[0][0])) for x in s)
    line = "%s: %d samples over %.0f s: MONOTONIC %+.1f ppm (%+.3f s), REALTIME %+.1f ppm; plain difference %+.1f ppm; largest offset from the fit %.3f s" % (
        f, len(s), span, (mono - 1) * 1e6, (mono - 1) * span, (real - 1) * 1e6, (((s[-1][2] - s[0][2]) / 1e9) / span - 1) * 1e6, off)
    if len(d) >= 3:
        t = [x[0] for x in d]
        hp = slope(t, [x[1] / 1e9 for x in d]); tsc = slope(t, [x[2] for x in d])
        tk = slope(t, [x[3] / 100 for x in d]); irq = slope(t, [x[4] for x in d]); add = slope(t, [x[5] / 100 for x in d])
        cal = d[-1][6] * 100
        line += "\n    HPET %+.1f ppm; TSC %.0f kHz (10 ms calibration %.0f kHz, %+.0f ppm); tick count %+.1f ppm; timer interrupts %.4f/s (%+.0f ppm from 100); ticks the interrupts added %+.1f ppm; forced advances %d" % (
            (hp - 1) * 1e6, tsc / 1e3, cal / 1e3, (cal - tsc) / tsc * 1e6, (tk - 1) * 1e6, irq, (irq / 100 - 1) * 1e6, (add - 1) * 1e6,
            (d[-1][3] - d[0][3]) - (d[-1][5] - d[0][5]))
    print(line)
