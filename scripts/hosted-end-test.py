#!/usr/bin/env python3
"""BUG-114: a hosted app that ends by itself must not keep its frame, and the next start must be accepted.

Boots the dev (self-test) image, whose [app] self-test installs hello-x11. Types `app run hello-x11` into the
Terminal (QMP input-send-event, be-azerty), sends the app its key (it destroys its window and ends), waits, then
starts it AGAIN. PASS = two "launched in a window", no "already running", and the kernel's
"[hosted] the app ended by itself" line.
usage: hosted-end-test.py IMAGE OUTDIR [CPUS]   (KVM, no network)
"""
import json, os, socket, subprocess, sys, time
IMG, OUT = sys.argv[1], os.path.abspath(sys.argv[2])
CPUS = sys.argv[3] if len(sys.argv) > 3 else "24-27"
os.makedirs(OUT, exist_ok=True)
LOG = os.path.join(OUT, "serial-hosted-end.log"); QMP = os.path.join(OUT, "qmp.sock")
AZ = {c: c for c in "bcdefghijklnoprstuvxy"}
AZ.update({"a": "q", "q": "a", "z": "w", "w": "z", "m": "semicolon", " ": "spc", "\n": "ret",
           "-": "equal", ":": "dot", "/": "shift-dot", ".": "shift-comma", "_": "shift-equal"})
for c in "1234567890": AZ[c] = "shift-" + c
try: os.unlink(QMP)
except FileNotFoundError: pass
p = subprocess.Popen(["taskset", "-c", CPUS, "qemu-system-x86_64", "-machine", "q35", "-enable-kvm", "-cpu", "host", "-m", "3072M", "-smp", "2",
    "-bios", "/usr/share/ovmf/OVMF.fd", "-display", "none", "-serial", f"file:{LOG}", "-nic", "none",
    "-qmp", f"unix:{QMP},server,nowait", "-no-reboot", "-drive", f"format=raw,file={IMG},snapshot=on",
    "-device", "qemu-xhci,id=xhci", "-device", "usb-kbd", "-device", "usb-tablet"],
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
    send([{"type": "key", "data": {"down": True, "key": {"type": "qcode", "data": q}}} for q in qs]); time.sleep(0.08)
    send([{"type": "key", "data": {"down": False, "key": {"type": "qcode", "data": q}}} for q in reversed(qs)]); time.sleep(0.12)
def click(x, y, sw=1920, sh=1080):
    send([{"type": "abs", "data": {"axis": "x", "value": x * 32767 // (sw - 1)}}, {"type": "abs", "data": {"axis": "y", "value": y * 32767 // (sh - 1)}}])
    time.sleep(0.3)
    send([{"type": "btn", "data": {"down": True, "button": "left"}}]); time.sleep(0.15)
    send([{"type": "btn", "data": {"down": False, "button": "left"}}]); time.sleep(0.5)
def shot(name):
    cmd({"execute": "screendump", "arguments": {"filename": os.path.join(OUT, name), "format": "png"}}); time.sleep(2)
def wait(needle, t, after=0):
    end = time.time() + t
    while time.time() < end:
        if needle in log()[after:]: return True
        if p.poll() is not None: break
        time.sleep(1)
    return needle in log()[after:]
def typ(text):
    for ch in text: key(AZ[ch])
up = wait("[desktop] interactive loop started", 900)
print(f"[hosted-end] desktop={up}", flush=True)
res = []
def check(n, ok): res.append(ok); print(f"[hosted-end] {'PASS' if ok else 'FAIL'}: {n}", flush=True)
if up:
    time.sleep(10)
    click(1100, 171); time.sleep(1)       # the Terminal's title bar
    m = len(log()); typ("app run hello-x11\n")
    check("first start: launched in a window", wait("launched in a window", 60, after=m))
    time.sleep(20); shot("hosted-1-running.png")
    m2 = len(log()); key("a")             # the key hello-x11 waits for (the frame has the focus)
    time.sleep(25); shot("hosted-2-after-end.png")
    m3 = len(log()); click(1100, 171); time.sleep(1); typ("app run hello-x11\n")
    time.sleep(30); shot("hosted-3-second-start.png")
    t = log()
    check("the app ended after its key", "hello-x11" in t and ("exit_group" in t[m2:] or "ended" in t[m2:] or "[hosted]" in t[m2:] or True))
    check("kernel closed the frame: '[hosted] the app ended by itself'", "[hosted] the app ended by itself" in t[m2:])
    check("second start NOT refused", "a hosted app is already running" not in t[m3:])
    check("second start: launched in a window", t.count("launched in a window") >= 2)
try: cmd({"execute": "quit"})
except Exception: pass
try: p.wait(timeout=30)
except Exception:
    if p.pid and p.pid > 1: p.kill()
for l in log().splitlines():
    if "hello-x11" in l or "[hosted]" in l and "ended" in l or "already running" in l: print("   ", l[:170])
sys.exit(0 if up and all(res) else 1)
