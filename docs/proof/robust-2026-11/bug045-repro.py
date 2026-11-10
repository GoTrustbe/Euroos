#!/usr/bin/env python3
"""Repro for BUG-045: a window dragged to the left edge is drawn over the dock,
and a click on the part of the window that covers a dock tile goes to the dock.

usage (from the repository root): bug045-repro.py IMAGE WORKDIR [--kvm]
Uses the Guest class of scripts/gui-smoke.py."""
import importlib.util, os, sys, types

spec = importlib.util.spec_from_file_location("gui_smoke", os.path.join("scripts", "gui-smoke.py"))
gs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gs)

kvm = "--kvm" in sys.argv
a = types.SimpleNamespace(image=sys.argv[1], workdir=os.path.abspath(sys.argv[2]), kvm=kvm,
                          cpu_opts="", smp=2, mem="2G", pin="", input="usb", net="none", layout="auto", extra="",
                          boot_timeout=300 if kvm else 1500, alive_timeout=30 if kvm else 180,
                          slow=1.0 if kvm else 3.0, soak=0, seed=0, skip_smoke=True, keep_image=False)
os.makedirs(a.workdir, exist_ok=True)
g = gs.Guest(a)
try:
    g.boot()
    g.close_all_windows()
    g.click(*gs.dock_tile(4))                 # the terminal
    g.nap(0.8)
    x, y = g.look().windows()[0]
    print(f"terminal window at {x},{y}")
    g.drag(x + 190, y + 22, 120, y + 60)      # pointer to x = 120: the window stops at x = 0
    covered = g.look("bug045-window-over-dock")
    print("after the drag, windows:", covered.windows())
    # (45, 349) is inside the terminal window now, and it is where the Settings tile is.
    g.click(*gs.dock_tile(5))
    g.nap(0.8)
    after = g.look("bug045-click-went-to-dock")
    print("after a click inside the window at the Settings tile's place, windows:", after.windows())
    print("BUG-045 reproduced" if len(after.windows()) > len(covered.windows()) else "not reproduced: the window took the click")
    g.alive("end")
finally:
    g.stop()
