#!/usr/bin/env python3
"""How long does the desktop take per frame while it repaints everything in every loop
iteration (notification list open), with a handful of windows up?

Measures (a) the round trip of an echo on the serial console, which the desktop loop
serves once per iteration, and (b) how long after a pointer move the picture has
changed. One guest.

usage: frame_time.py GUI_SMOKE.py IMAGE WORKDIR [--kvm] [--pin A-B]
Uses the Guest class of gui-smoke.py."""
import importlib.util, os, statistics, sys, time, types

gsp, image, wd = sys.argv[1], sys.argv[2], os.path.abspath(sys.argv[3])
kvm = "--kvm" in sys.argv
opt = lambda n, d: sys.argv[sys.argv.index(n) + 1] if n in sys.argv else d
spec = importlib.util.spec_from_file_location("gui_smoke", gsp)
gs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gs)
a = types.SimpleNamespace(image=image, workdir=wd, kvm=kvm, cpu_opts="", smp=4, mem="4G", pin=opt("--pin", ""),
                          input="usb", net="none", layout="auto", extra="",
                          boot_timeout=300 if kvm else 1500, alive_timeout=30 if kvm else 240,
                          slow=1.0 if kvm else 3.0, soak=0, seed=0, skip_smoke=True, keep_image=False)
os.makedirs(wd, exist_ok=True)
g = gs.Guest(a)


def echo_ms(n=15):
    out = []
    for i in range(n):
        tok = f"ft-{int(time.time() * 1000) % 100000}-{i}"
        pos = g.serial.mark()
        t0 = time.time()
        g.serial.send_line("echo " + tok)
        if not g.serial.wait_for("[scon] " + tok, 30, pos):
            out.append(30000.0)
            break
        out.append((time.time() - t0) * 1000)
        time.sleep(0.05)
    return out


def pointer_lag():
    """seconds until a pointer move shows on a screendump (polled every 50 ms, up to 5 s)"""
    g.move(g.w // 2, g.h // 2)
    time.sleep(1.5)
    before = g.shot()
    g.move(g.w // 2 + 150, g.h // 2 + 90)
    t0 = time.time()
    while time.time() - t0 < 5:
        if g.shot().differs(before, min_pixels=4):
            return time.time() - t0
        time.sleep(0.05)
    return None


def report(tag):
    e = echo_ms()
    lags = [pointer_lag() for _ in range(5)]
    ls = ", ".join("none in 5 s" if v is None else f"{v * 1000:.0f} ms" for v in lags)
    print(f"FRAME-TIME {tag}: console echo median {statistics.median(e):.0f} ms, max {max(e):.0f} ms; pointer visible after: {ls}", flush=True)


try:
    g.boot()
    g.close_all_windows()
    report("empty desktop")
    for i in (0, 1, 4, 5, 6, 8, 9):          # files, notes, terminal, settings, calculator, text, monitor
        g.click(*gs.dock_tile(i))
        g.nap(0.5)
    report("7 windows")
    g.click(g.w - 160, 50)                    # the status panel: the notification list
    g.nap(1.0)
    g.shot("frame-time-notifications-open")
    report("7 windows + notification list open (full repaint in every iteration)")
    g.alive("end")
finally:
    g.stop()
