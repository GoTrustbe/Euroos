#!/usr/bin/env python3
"""Does the desktop survive a window dragged to the bottom edge of the screen?
One guest per dock app: open the app, drag its title bar to y = Y (default 1050 on a
1080-line screen), then ask the serial console for an echo.

usage: drag_bottom.py GUI_SMOKE.py IMAGE WORKDIR [--kvm] [--y N] [--apps 0,1,...] [--pin A-B]
Uses the Guest class of gui-smoke.py."""
import importlib.util, os, sys, types

gsp, image, wd = sys.argv[1], sys.argv[2], os.path.abspath(sys.argv[3])
kvm = "--kvm" in sys.argv
opt = lambda n, d: sys.argv[sys.argv.index(n) + 1] if n in sys.argv else d
Y = int(opt("--y", "1050"))
apps = [int(v) for v in opt("--apps", "0,1,2,3,4,5,6,7,8,9,10").split(",")]
pin = opt("--pin", "")
NAMES = ["files", "notes", "clock", "browser", "terminal", "settings", "store", "star", "text", "monitor", "log"]

spec = importlib.util.spec_from_file_location("gui_smoke", gsp)
gs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gs)

bad = []
for i in apps:
    a = types.SimpleNamespace(image=image, workdir=os.path.join(wd, NAMES[i]), kvm=kvm, cpu_opts="", smp=2, mem="2G",
                              pin=pin, input="usb", net="none", layout="auto", extra="",
                              boot_timeout=300 if kvm else 1500, alive_timeout=20 if kvm else 180,
                              slow=1.0 if kvm else 3.0, soak=0, seed=0, skip_smoke=True, keep_image=False)
    os.makedirs(a.workdir, exist_ok=True)
    g = gs.Guest(a)
    verdict = "?"
    try:
        g.boot()
        g.close_all_windows()
        g.click(*gs.dock_tile(i))
        g.nap(0.8)
        wins = g.look().windows()
        if not wins:
            verdict = "no window found"
        else:
            x, y = wins[0]
            g.drag(x + 190, y + 22, x + 190, Y)
            g.nap(1.0)
            g.shot("dragged-to-bottom")
            g.alive("after the drag")
            verdict = "alive"
    except gs.Fatal as e:
        verdict = f"HANG/CRASH: {e}"
        bad.append(NAMES[i])
    finally:
        g.stop()
    print(f"DRAG-BOTTOM {NAMES[i]}: window to y={Y}: {verdict}", flush=True)
print("DRAG-BOTTOM RESULT:", "all alive" if not bad else "hang with " + ", ".join(bad), flush=True)
sys.exit(1 if bad else 0)
