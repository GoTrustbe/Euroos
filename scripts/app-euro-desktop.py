#!/usr/bin/env python3
"""A third-party NATIVE app on the DESKTOP (sprint WP6).

Boots the dev image (its [app] self-test installs hello-euro (Rust, no_std, Euro API only), signed with the
example developer key), waits for the desktop, types `app run hello-euro` into
the Terminal over QMP input-send-event (USB keyboard + tablet, the route a person's
input takes; be-azerty layout) after a click on the Terminal, screendumps the desktop to
PNG and reports whether the kernel launched the app in a hosted window.
Usage (as root): python3 scripts/app-x11-desktop.py [outdir]
"""
import json, os, socket, subprocess, sys, time

OUT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/euroos-appx11"
os.makedirs(OUT, exist_ok=True)
LOG = os.path.join(OUT, "serial.log"); QMP = os.path.join(OUT, "qmp.sock")
WAIT = int(os.environ.get("WAIT", "60"))
APP = os.environ.get("APP", "hello-euro")  # hello-euro (native) or hello-euro-wasm
NET = os.environ.get("NET", "") == "1"      # attach user-mode networking (the app store needs it)
# CMDS: extra Terminal commands typed before `app run`, separated by ';' (e.g. the store flow)
CMDS = [c.strip() for c in os.environ.get("CMDS", "").split(";") if c.strip()]
RAW_CMD = os.environ.get("RAW_CMD", "")   # typed instead of `app run APP` (e.g. an rdp command)

# be-azerty: letters a/q, z/w swapped, m on the qwerty semicolon key, digits shifted,
# '-' on the qwerty equal key (same table as scripts/qmp-input.py).
AZ = {c: c for c in "bcdefghijklnoprstuvxy"}
AZ.update({"a": "q", "q": "a", "z": "w", "w": "z", "m": "semicolon", " ": "spc", "\n": "ret",
           "-": "equal", ":": "dot", "/": "shift-dot", ".": "shift-comma", "_": "shift-equal"})
for c in "1234567890":
    AZ[c] = "shift-" + c

try: os.unlink(QMP)
except FileNotFoundError: pass
p = subprocess.Popen(["qemu-system-x86_64", "-machine", "q35", "-cpu", "qemu64,+smep,+smap", "-m", "3072M",
    "-bios", "/usr/share/ovmf/OVMF.fd", "-display", "none", "-serial", f"file:{LOG}",
    "-qmp", f"unix:{QMP},server,nowait", "-monitor", f"unix:{QMP}.mon,server,nowait", "-no-reboot", "-drive", "format=raw,file=eurokernel.img",
    "-device", "qemu-xhci,id=xhci", "-device", "usb-kbd", "-device", "usb-tablet"]
    + (["-netdev", "user,id=n0", "-device", "virtio-net-pci,netdev=n0"] if NET else []),
    stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
for _ in range(100):
    if os.path.exists(QMP): break
    time.sleep(0.2)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(QMP); f = s.makefile("rwb", buffering=0)
def cmd(o): f.write((json.dumps(o) + "\n").encode()); return json.loads(f.readline().decode())
f.readline(); cmd({"execute": "qmp_capabilities"})
def log():
    try: return open(LOG, errors="replace").read()
    except FileNotFoundError: return ""
def send(ev): cmd({"execute": "input-send-event", "arguments": {"events": ev}})
def key(qc):
    qs = qc.split("-")
    send([{"type": "key", "data": {"down": True, "key": {"type": "qcode", "data": q}}} for q in qs])
    time.sleep(0.08)
    send([{"type": "key", "data": {"down": False, "key": {"type": "qcode", "data": q}}} for q in reversed(qs)])
    time.sleep(0.12)
def click(x, y, sw=1920, sh=1080):
    send([{"type": "abs", "data": {"axis": "x", "value": x * 32767 // (sw - 1)}},
          {"type": "abs", "data": {"axis": "y", "value": y * 32767 // (sh - 1)}}])
    time.sleep(0.3)
    send([{"type": "btn", "data": {"down": True, "button": "left"}}]); time.sleep(0.15)
    send([{"type": "btn", "data": {"down": False, "button": "left"}}]); time.sleep(0.5)
def shot(name):
    cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(os.path.join(OUT, name)), "format": "png"}})
    time.sleep(2)

t0 = time.time(); ok = False
while time.time() - t0 < 1500:
    if "[desktop] interactive loop started" in log(): ok = True; break
    if "PANIC" in log() or p.poll() is not None: break
    time.sleep(5)
print(f"desktop={ok} at {int(time.time()-t0)}s", flush=True)
if ok:
    time.sleep(15)
    shot("desktop.png")
    click(1100, 171)  # the Terminal's title bar: give it keyboard focus
    time.sleep(1)
    for c in CMDS:
        for ch in c + "\n":
            key(AZ[ch])
        print(f"typed {c!r}", flush=True)
        time.sleep(int(os.environ.get("CMD_WAIT", "25")))
    for ch in (RAW_CMD + "\n") if RAW_CMD else f"app run {APP}\n":
        key(AZ[ch])
    print(f"typed at {int(time.time()-t0)}s", flush=True)
    time.sleep(WAIT)
    shot("app.png")
try: cmd({"execute": "quit"})
except Exception: pass
p.wait(timeout=30)
txt = log()
launched = "launched natively" in txt or "launched in the WASM sandbox" in txt or "[rdp] session ACTIVE" in txt
print("launched-in-window:", launched)
for l in txt.splitlines():
    if APP in l or "[euroapp" in l or "launch failed" in l or "[euroapi]" in l or "[rdp]" in l:
        print("   ", l[:160])
sys.exit(0 if ok and launched else 1)
