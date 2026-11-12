#!/usr/bin/env python3
"""seam.py SCREENSHOT.png [...]: BUG-054. For every screenshot of the empty desktop, the colour
step between horizontally neighbouring pixels at the middle of the screen (x = w/2 - 1 | w/2)
against the step at every other column, over the rows between the dock and the status panel
that hold only wallpaper. A step is the sum of the absolute differences of R, G and B."""
import importlib.util, os, sys
spec = importlib.util.spec_from_file_location("montage_lib", os.path.join(os.path.dirname(os.path.abspath(__file__)), "montage.py"))
src = open(spec.origin, encoding="utf-8").read().split("out, cols = sys.argv[1]")[0]     # the PNG reader only
ns = {}
exec(src, ns)
for path in sys.argv[1:]:
    w, h, rows = ns["read_png"](path)
    x0, x1 = 100, w - 320                      # right of the dock, left of the status panel
    step = lambda r, x: sum(abs(r[x * 3 + c] - r[(x - 1) * 3 + c]) for c in range(3))
    mid = [step(rows[y], w // 2) for y in range(20, h - 40)]
    others = {}
    for y in range(20, h - 40, 4):
        r = rows[y]
        for x in range(x0 + 1, x1):
            if x != w // 2:
                s = step(r, x)
                if s > others.get(x, 0):
                    others[x] = s
    worst_other = max(others.values())
    typical = sorted(others.values())[len(others) // 2]
    print(f"{path}: {w}x{h}; step at x={w // 2}: max {max(mid)}, mean {sum(mid) / len(mid):.2f} over {len(mid)} rows; "
          f"other columns {x0 + 1}..{x1 - 1}: median of the per-column maxima {typical}, largest {worst_other}")
