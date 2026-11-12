#!/usr/bin/env python3
"""Scripted look at the desktop: boot an image with the Guest class of gui-smoke.py, run
a list of input steps, keep screenshots and what the serial console answered.

usage: probe.py GUI_SMOKE.py IMAGE WORKDIR SCENARIO [--kvm] [--pin A-B] [--input usb|virtio]
                [--bootopt "k=v;k=v"] [--extra "QEMU ARGS"] [--smp N] [--mem 2G] [--keep-tour]

--bootopt writes \\EFI\\BOOT\\euroos-boot.conf into a private copy of the image (mtools),
e.g. --bootopt display.mode=1280x720. The firmware must offer the mode: add
--extra "-device VGA,xres=1280,yres=720 -vga none" style arguments when it does not.

Scenario lines (# comments, blank lines ignored):
  closeall                 close every window found on the screen
  dock N                   click dock tile N (0 = files ... 10 = log)
  click X Y | rclick X Y | dclick X Y | move X Y
  drag X0 Y0 X1 Y1
  panel                    click the status panel (top right): the notification list
  desk | rdesk             left / right click on the bare desktop (lower middle)
  wshrink                  drag the resize grip of the window opened by the last `dock N`
                           towards 200 x 150 (the desktop stops at the app's minimum)
  wizclose                 close the install wizard's window
  keydown QCODE | keyup QCODE
  wclick DX DY             click at an offset from the top-left of the first window found
  wrclick DX DY | wmove DX DY | wdrag DX0 DY0 DX1 DY1 | wresize W H CURW CURH
  key QCODE[+QCODE...]     e.g. key esc, key alt_r+2 (held together)
  type TEXT                through the layout table of gui-smoke
  wheel N                  N notches, positive = towards the user (scroll down)
  scon COMMAND             run a shell command over the serial console, print its [scon] lines
  nap SECONDS
  shot NAME                NAME.png in the work directory
  expect TEXT              TEXT must be in the serial log since the previous mark
  absent TEXT              TEXT must not be in the serial log since the previous mark
  mark
  echo TEXT
Positions are pixels of the guest screen. Exit status 1 when an expect/absent failed."""
import importlib.util, os, subprocess, sys, time, types

argv = sys.argv[1:]
def opt(name, default=None, flag=False):
    if name in argv:
        i = argv.index(name)
        if flag:
            argv.pop(i)
            return True
        v = argv[i + 1]
        del argv[i:i + 2]
        return v
    return False if flag else default

kvm = opt("--kvm", flag=True)
keep_tour = opt("--keep-tour", flag=True)
pin = opt("--pin", "")
inp = opt("--input", "usb")
bootopt = opt("--bootopt", "")
extra = opt("--extra", "")
smp = int(opt("--smp", "2"))
mem = opt("--mem", "2G")
gsp, image, wd, scenario = argv[0], argv[1], os.path.abspath(argv[2]), argv[3]
os.makedirs(wd, exist_ok=True)
spec = importlib.util.spec_from_file_location("gui_smoke", gsp)
gs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gs)

if bootopt:
    src = os.path.join(wd, "image-with-options.img")
    subprocess.run(["cp", "--sparse=always", image, src], check=True)
    conf = os.path.join(wd, "euroos-boot.conf")
    open(conf, "w").write("".join(l.strip() + "\n" for l in bootopt.split(";") if l.strip()))
    subprocess.run(["mcopy", "-o", "-i", src, conf, "::/EFI/BOOT/euroos-boot.conf"], check=True)
    image = src

a = types.SimpleNamespace(image=image, workdir=wd, kvm=kvm, cpu_opts="", smp=smp, mem=mem, pin=pin,
                          input=inp, net="none", layout="auto", extra=extra,
                          boot_timeout=300 if kvm else 1500, alive_timeout=30 if kvm else 240,
                          slow=1.0 if kvm else 3.0, soak=0, seed=0, skip_smoke=True, keep_image=False)
g = gs.Guest(a)
if keep_tour:
    # boot() closes the tour with Esc when the log says it is up; keep it for a picture.
    _key = g.key
    g.key = lambda *q, **k: None if q == ("esc",) and not getattr(g, "booted", False) else _key(*q, **k)
failed = 0
mark = 0
last_dock = 0
# Default window sizes per dock tile (main.rs), as laid out for 1920 x 1080.
DEFAULT_SIZE = [(720, 620), (720, 480), (600, 430), (900, 730), (800, 740), (760, 560),
                (660, 560), (780, 600), (760, 600), (620, 560), (700, 560)]

def fitted(i):
    """The size main.rs::fit_window gives the window of dock tile i on this screen."""
    w, h = DEFAULT_SIZE[i]
    panel_x = g.w - 300
    right = panel_x if panel_x >= gs.SIDEBAR_W + 640 else g.w
    return min(w, right - gs.SIDEBAR_W - 8), min(h, g.h - 34 - 8)

def win():
    w = g.look().windows()
    if not w:
        raise SystemExit("no window found on the screen")
    return w[0]

def scon(cmdline):
    pos = g.serial.mark()
    g.serial.send_line(cmdline)
    tok = f"probe-done-{int(time.time() * 1000) % 1000000}"
    time.sleep(0.3)
    g.serial.send_line("echo " + tok)
    g.serial.wait_for("[scon] " + tok, a.alive_timeout, pos)
    out = g.serial.since(pos)
    for l in out.splitlines():
        if tok not in l:
            print("    | " + l, flush=True)

try:
    g.boot()
    g.booted = True
    print(f"screen {g.w}x{g.h}", flush=True)
    for raw in open(scenario):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        op, _, rest = line.partition(" ")
        p = rest.split()
        print("> " + line, flush=True)
        if op == "closeall":
            g.close_all_windows()
        elif op == "dock":
            last_dock = int(p[0])
            g.click(*gs.dock_tile(last_dock))
        elif op == "panel":
            g.click(g.w - gs.PANEL_W // 2, 60)
        elif op in ("desk", "rdesk"):
            g.click(gs.SIDEBAR_W + (g.w - gs.SIDEBAR_W - gs.PANEL_W) // 2, g.h * 3 // 4, button="right" if op == "rdesk" else "left")
        elif op == "wshrink":
            x, y = win()
            cw, ch = fitted(last_dock) if "--nofit" not in sys.argv else DEFAULT_SIZE[last_dock]
            g.drag(x + cw - 6, y + ch - 6, x + 200, y + 150, steps=12)
        elif op == "click":
            g.click(int(p[0]), int(p[1]))
        elif op == "rclick":
            g.click(int(p[0]), int(p[1]), button="right")
        elif op == "dclick":
            g.click(int(p[0]), int(p[1]), double=True)
        elif op == "move":
            g.move(int(p[0]), int(p[1]))
        elif op == "drag":
            g.drag(*[int(v) for v in p[:4]])
        elif op in ("wclick", "wrclick", "wmove"):
            x, y = win()
            if op == "wmove":
                g.move(x + int(p[0]), y + int(p[1]))
            else:
                g.click(x + int(p[0]), y + int(p[1]), button="right" if op == "wrclick" else "left")
        elif op == "wdrag":
            x, y = win()
            g.drag(x + int(p[0]), y + int(p[1]), x + int(p[2]), y + int(p[3]))
        elif op == "wresize":
            # drag the grip in the bottom-right corner of a CURW x CURH window to W x H
            x, y = win()
            w, h, cw, ch = [int(v) for v in p[:4]]
            g.drag(x + cw - 6, y + ch - 6, x + w - 6, y + h - 6, steps=12)
        elif op == "wizclose":
            # the install wizard opens at x = SIDEBAR_W + 16 (main.rs); close that window
            ws = [w for w in g.look().windows() if abs(w[0] - (gs.SIDEBAR_W + 16)) <= 3]
            if not ws:
                raise SystemExit("no install wizard window found")
            g.click(ws[0][0] + 20, ws[0][1] + 22)
        elif op in ("keydown", "keyup"):
            g._send([{"type": "key", "data": {"down": op == "keydown", "key": {"type": "qcode", "data": p[0]}}}])
        elif op == "key":
            g.key(*rest.strip().split("+"))
        elif op == "type":
            g.type(rest)
        elif op == "wheel":
            n = int(p[0])
            for _ in range(abs(n)):
                b = "wheel-down" if n > 0 else "wheel-up"
                g._send([{"type": "btn", "data": {"down": True, "button": b}}])
                g._send([{"type": "btn", "data": {"down": False, "button": b}}])
                time.sleep(0.08 * a.slow)
        elif op == "scon":
            scon(rest)
        elif op == "nap":
            g.nap(float(p[0]))
        elif op == "shot":
            g.nap(0.6)
            g.shot(p[0])
        elif op == "mark":
            mark = g.serial.mark()
        elif op in ("expect", "absent"):
            g.nap(0.5)
            found = rest in g.serial.since(mark)
            ok = found if op == "expect" else not found
            print(f"  {'ok  ' if ok else 'FAIL'} {op}: {rest}", flush=True)
            failed += 0 if ok else 1
        elif op == "echo":
            print(rest, flush=True)
        else:
            raise SystemExit(f"unknown step: {line}")
    g.alive("end of the scenario")
    print(f"PROBE {'PASS' if not failed else 'FAIL'} ({failed} failed check(s)), max console latency {g.max_echo_ms} ms", flush=True)
except gs.Fatal as e:
    print(f"PROBE CRASH/HANG: {e}", flush=True)
    failed += 1
finally:
    g.stop()
sys.exit(1 if failed else 0)
