#!/usr/bin/env python3
"""Integration driver (outside the tree): one boot, Chromium, then only the public
Wikimedia video step of chrome-mem-soak.py (video_play), N times. Uses the harness
as a library, unchanged. usage: wmvideo.py --image I --pack P --nss N --workdir D --pin C [--seconds 90] [--times 2]"""
import importlib.util, os, sys, time, argparse
S = "/srv/euroos/final-runs/scripts"
def load(n, f):
    sp = importlib.util.spec_from_file_location(n, os.path.join(S, f)); m = importlib.util.module_from_spec(sp); sp.loader.exec_module(m); return m
ms = load("chrome_mem_soak", "chrome-mem-soak.py")
cs = ms.cs
ap = argparse.ArgumentParser()
for k, d in (("--image", None), ("--pack", None), ("--nss", None), ("--workdir", None), ("--pin", ""), ("--mem", "6144M"), ("--input", "usb"), ("--nic", "virtio-net-pci"), ("--extra", "")):
    ap.add_argument(k, default=d)
ap.add_argument("--kvm", action="store_true"); ap.add_argument("--smp", type=int, default=2)
ap.add_argument("--url", default=""); ap.add_argument("--seconds", type=int, default=90); ap.add_argument("--times", type=int, default=2)
a = ap.parse_args()
a.only, a.rounds, a.soak_hours, a.starts, a.sites = "", 1, 0.0, 0, ""
a.workdir = os.path.abspath(a.workdir); os.makedirs(a.workdir, exist_ok=True)
t0 = time.time()
def body(g):
    if not g.start_chrome("file:///tmp/input-test.html"):
        return
    g.close_infobars(); g.move(g.w - 4, g.h - 4); g.calibrate_idle_icon()
    for r in range(1, a.times + 1):
        ms.video_play(g, a.url or ms.FREE_VIDEO, a.seconds, r)
        try:
            st = g.js_state() if hasattr(g, "js_state") else None
        except Exception:
            st = None
g, verdict, rc = cs.one_boot(a, a.workdir, body)
cs.write_report(a, a.workdir, g, verdict, t0, {})
print(f"[wmvideo] {verdict}", flush=True)
sys.exit(rc)
