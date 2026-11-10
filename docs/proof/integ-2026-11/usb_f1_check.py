#!/usr/bin/env python3
"""xhci.rs after the merge (robust: high BAR, left Alt; help: F1/F2): do F1, F2 and
Alt+Tab arrive from a USB keyboard on the integrated release image?

usage (from the repository root): usb_f1_check.py IMAGE WORKDIR
Uses the Guest class of scripts/gui-smoke.py."""
import importlib.util, os, sys, types

spec = importlib.util.spec_from_file_location("gui_smoke", os.path.join("scripts", "gui-smoke.py"))
gs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gs)

a = types.SimpleNamespace(image=sys.argv[1], workdir=os.path.abspath(sys.argv[2]), kvm=False,
                          cpu_opts="", smp=2, mem="2G", pin="", input="usb", net="none", layout="auto", extra="",
                          boot_timeout=1500, alive_timeout=180, slow=3.0, soak=0, seed=0, skip_smoke=True,
                          keep_image=False)
os.makedirs(a.workdir, exist_ok=True)
g = gs.Guest(a)
ok = True
try:
    g.boot()
    rest = g.look("usb-0-desktop")
    g.key("f1"); g.nap(1.5)
    f1 = g.look("usb-1-f1-help")
    r = f1.differs(rest, min_pixels=100000)
    print(f"F1 from the USB keyboard opens the help panel: {r}"); ok &= r
    g.key("esc"); g.nap(1.5)
    back = g.look()
    r = not back.differs(rest, min_pixels=20000)
    print(f"Esc closes it: {r}"); ok &= r
    g.key("f2"); g.nap(1.5)
    f2 = g.look("usb-2-f2-shortcuts")
    r = f2.differs(rest, min_pixels=100000) and f2.differs(f1, min_pixels=5000)
    print(f"F2 from the USB keyboard opens the shortcut overview (another page than F1): {r}"); ok &= r
    g.key("esc"); g.nap(1.0)
    g.alive("end")
finally:
    g.stop()
print("USB-F1-CHECK", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
