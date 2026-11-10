#!/usr/bin/env python3
"""Repro for BUG-057: the calculator resized to the smallest height the desktop allows
(200 px) freezes the desktop loop for good.

usage: bug057-repro.py GUI_SMOKE.py IMAGE WORKDIR [--kvm] [--pin A-B] [--to-h N] [--to-w N]
Uses the Guest class of scripts/gui-smoke.py. EuroReken opens at w 660, h 560 (main.rs)."""
import importlib.util, os, sys, types

gsp, image, wd = sys.argv[1], sys.argv[2], os.path.abspath(sys.argv[3])
kvm = "--kvm" in sys.argv
opt = lambda n, d: sys.argv[sys.argv.index(n) + 1] if n in sys.argv else d
to_h, to_w = int(opt("--to-h", "150")), int(opt("--to-w", "660"))
spec = importlib.util.spec_from_file_location("gui_smoke", gsp)
gs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gs)

a = types.SimpleNamespace(image=image, workdir=wd, kvm=kvm, cpu_opts="", smp=2, mem="2G", pin=opt("--pin", ""),
                          input="usb", net="none", layout="auto", extra="",
                          boot_timeout=300 if kvm else 1500, alive_timeout=20 if kvm else 240,
                          slow=1.0 if kvm else 3.0, soak=0, seed=0, skip_smoke=True, keep_image=False)
os.makedirs(wd, exist_ok=True)
g = gs.Guest(a)
rc = 0
try:
    g.boot()
    g.close_all_windows()
    g.click(*gs.dock_tile(6))                 # the calculator
    g.nap(0.8)
    x, y = g.look("bug057-0-calculator").windows()[0]
    print(f"calculator window at {x},{y} (660 x 560)")
    g.alive("before the resize")
    # the 18 px grip in the bottom-right corner, dragged to (x + to_w, y + to_h)
    g.drag(x + 660 - 6, y + 560 - 6, x + to_w - 6, y + to_h)
    g.nap(1.5)
    g.shot("bug057-1-after-resize")
    g.alive("after the resize")
    print(f"resized towards {to_w} x {to_h}: the desktop answers -> not reproduced / fixed")
except gs.Fatal as e:
    print(f"resized towards {to_w} x {to_h}: {e} -> BUG-057 reproduced")
    rc = 2
finally:
    g.stop()
sys.exit(rc)
