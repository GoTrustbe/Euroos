#!/usr/bin/env python3
"""The live GTK demo window (ggtk) on the desktop of a self-test image built with
the feature live-demos: screenshots while the kernel's own demo test focuses the
window, types into its entry and clicks its button, plus keys sent over USB-HID
(QMP) while the window has the focus.

usage: gtk-live.py IMAGE WORKDIR [--kvm] [--pin CPUS] [--keys TEXT]
Writes WORKDIR/serial.log, WORKDIR/shot-NNN.png (one every 0.25 s from the moment
the app is started until it is closed) and prints the lines of the app (GGTK:) and
of the demo test ([gtk-test]) with the number of the screenshot taken after each.
QEMU has `-nic none` and is stopped through its own process handle.
"""
import json, os, socket, subprocess, sys, threading, time

args = [x for x in sys.argv[1:] if not x.startswith("--")]
IMG, W = args[0], os.path.abspath(args[1])
KVM = "--kvm" in sys.argv
PIN = sys.argv[sys.argv.index("--pin") + 1] if "--pin" in sys.argv else ""
KEYS = sys.argv[sys.argv.index("--keys") + 1] if "--keys" in sys.argv else "ok"
os.makedirs(W, exist_ok=True)
img = os.path.join(W, "boot.img"); subprocess.run(["cp", "--sparse=always", IMG, img], check=True)
qmp = os.path.join(W, "qmp.sock")
if os.path.exists(qmp): os.unlink(qmp)
acc = ["-enable-kvm", "-cpu", "host"] if KVM else ["-cpu", "qemu64,+smep,+smap"]
cmd = ["qemu-system-x86_64", "-machine", "q35", *acc, "-m", "4G", "-smp", "2", "-bios", "/usr/share/ovmf/OVMF.fd",
       "-drive", f"format=raw,file={img}", "-display", "none", "-vga", "std", "-serial", "stdio", "-no-reboot", "-nic", "none",
       "-device", "qemu-xhci,id=xhci", "-device", "usb-kbd", "-device", "usb-tablet", "-qmp", f"unix:{qmp},server,nowait"]
if PIN: cmd = ["taskset", "-c", PIN] + cmd
p = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
lines = []; shots = [0]
logf = open(os.path.join(W, "serial.log"), "w")
def reader():
    for raw in p.stdout:
        l = raw.decode("utf-8", "replace").rstrip("\r\n")
        lines.append((l, shots[0])); logf.write(l + "\n"); logf.flush()
threading.Thread(target=reader, daemon=True).start()
for _ in range(100):
    if os.path.exists(qmp): break
    time.sleep(0.2)
s = socket.socket(socket.AF_UNIX); s.connect(qmp); f = s.makefile("rwb", buffering=0); f.readline()
def q(o):
    f.write((json.dumps(o) + "\n").encode())
    while True:
        r = json.loads(f.readline().decode())
        if "return" in r or "error" in r: return r
q({"execute": "qmp_capabilities"})
def seen(needle): return any(needle in l for l, _ in lines)
def wait(needle, tmo):
    end = time.time() + tmo
    while time.time() < end and p.poll() is None:
        if seen(needle): return True
        time.sleep(0.05)
    return seen(needle)
def key(k):
    for down in (True, False):
        q({"execute": "input-send-event", "arguments": {"events": [{"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": k}}}]}})
        time.sleep(0.04)
def shot():
    shots[0] += 1
    q({"execute": "screendump", "arguments": {"filename": os.path.join(W, f"shot-{shots[0]:03}.png"), "format": "png"}})

live = wait("ggtk live on task", 1500 if not KVM else 600)
print(f"app started: {live}", flush=True)
typed = False
end = time.time() + (240 if KVM else 900)
while live and time.time() < end and p.poll() is None:
    shot()
    if not typed and seen("[gtk-test] focused + typed"):
        # the window has the keyboard focus now: real keys over USB-HID
        for ch in KEYS: key(ch)
        typed = True
        print(f"keys {KEYS!r} sent over USB-HID after shot {shots[0]}", flush=True)
    if seen("ggtk: error while loading"):
        print("the app did not load its libraries", flush=True); break
    if seen("terminated + arena freed") or seen("[gtk-test] closed"):
        time.sleep(1); shot(); break
    time.sleep(0.25)
time.sleep(2); shot()
p.terminate()
try: p.wait(timeout=10)
except Exception: p.kill()
time.sleep(0.5); logf.close(); os.unlink(img)
for l, n in lines:
    if "GGTK:" in l or "[gtk-test]" in l or "ggtk" in l or "PANIC" in l or "panicked" in l or "[xserver] unhandled" in l or "BadImplementation" in l:
        print(f"shot<={n:03} {l[:200]}")
print(f"screenshots: {shots[0]}")
