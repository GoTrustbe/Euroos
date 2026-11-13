#!/usr/bin/env python3
"""Boot an image and drive its desktop by hand-written steps: USB keyboard and
tablet over QMP (input-send-event), shell commands over the serial console, and
screenshots.

usage: drive.py IMAGE WORKDIR [--kvm] [--pin CPUS] [--mem 4G] -- STEP...
steps:  wait:TEXT[:SECONDS]   until TEXT is on the serial log (default 600 s)
        sleep:SECONDS
        scon:COMMAND          a shell command on the serial console; its output is printed
        type:TEXT             typed on the USB keyboard (US layout; \\n is Enter)
        key:QCODE[-QCODE]     one key or chord, e.g. key:ret, key:alt-tab
        click:X,Y             left click at screen pixel X,Y (1920x1080)
        shot:NAME             screendump to WORKDIR/NAME.png
        grep:TEXT             print the serial lines since the previous step that contain TEXT
QEMU has `-nic none`, lives in WORKDIR and is stopped through its own process handle.
"""
import json, os, socket, subprocess, sys, threading, time

i = sys.argv.index("--")
opts, steps = sys.argv[1:i], sys.argv[i + 1:]
pos = [x for x in opts if not x.startswith("--") and opts[opts.index(x) - 1] not in ("--pin", "--mem")]
IMG, W = pos[0], os.path.abspath(pos[1])
KVM = "--kvm" in opts
PIN = opts[opts.index("--pin") + 1] if "--pin" in opts else ""
MEM = opts[opts.index("--mem") + 1] if "--mem" in opts else "4G"
os.makedirs(W, exist_ok=True)
img = os.path.join(W, "boot.img"); subprocess.run(["cp", "--sparse=always", IMG, img], check=True)
qmp = os.path.join(W, "qmp.sock")
if os.path.exists(qmp): os.unlink(qmp)
acc = ["-enable-kvm", "-cpu", "host"] if KVM else ["-cpu", "qemu64,+smep,+smap"]
cmd = ["qemu-system-x86_64", "-machine", "q35", *acc, "-m", MEM, "-smp", "2", "-bios", "/usr/share/ovmf/OVMF.fd",
       "-drive", f"format=raw,file={img}", "-display", "none", "-vga", "std", "-serial", "stdio", "-no-reboot", "-nic", "none",
       "-device", "qemu-xhci,id=xhci", "-device", "usb-kbd", "-device", "usb-tablet", "-qmp", f"unix:{qmp},server,nowait"]
if PIN: cmd = ["taskset", "-c", PIN] + cmd
p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
lines = []
logf = open(os.path.join(W, "serial.log"), "w")
def reader():
    for raw in p.stdout:
        l = raw.decode("utf-8", "replace").rstrip("\r\n")
        lines.append(l); logf.write(l + "\n"); logf.flush()
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
def send(ev): q({"execute": "input-send-event", "arguments": {"events": ev}})
def key(qc):
    qs = qc.split("-")
    send([{"type": "key", "data": {"down": True, "key": {"type": "qcode", "data": k}}} for k in qs]); time.sleep(0.06)
    send([{"type": "key", "data": {"down": False, "key": {"type": "qcode", "data": k}}} for k in reversed(qs)]); time.sleep(0.1)
QMAP = {" ": "spc", "\n": "ret", "-": "minus", "_": "shift-minus", ".": "dot", "/": "slash", ",": "comma", ":": "shift-semicolon", "=": "equal"}
for c in "abcdefghijklmnopqrstuvwxyz0123456789": QMAP[c] = c
for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ": QMAP[c] = "shift-" + c.lower()
def wait(needle, tmo, after=0):
    end = time.time() + tmo
    while time.time() < end and p.poll() is None:
        if any(needle in l for l in lines[after:]): return True
        time.sleep(0.1)
    return any(needle in l for l in lines[after:])

mark = 0
for st in steps:
    kind, _, arg = st.partition(":")
    here = len(lines)
    if kind == "wait":
        text, _, tmo = arg.rpartition(":") if arg.rsplit(":", 1)[-1].isdigit() else (arg, "", "600")
        print(f"wait {text!r}: {wait(text, int(tmo))}", flush=True)
    elif kind == "sleep": time.sleep(float(arg))
    elif kind == "scon":
        p.stdin.write((arg + "\n").encode()); p.stdin.flush()
        wait("[scon-ready]", 120, after=here)
        print(f"scon $ {arg}", flush=True)
        for l in lines[here:]:
            if l.startswith("[scon] ") and not l.startswith("[scon] $ "): print("   " + l[7:200], flush=True)
    elif kind == "type":
        for ch in arg.replace("\\n", "\n"): key(QMAP[ch])
        print(f"typed {arg!r}", flush=True)
    elif kind == "key": key(arg); print(f"key {arg}", flush=True)
    elif kind == "click":
        x, y = (int(v) for v in arg.split(","))
        send([{"type": "abs", "data": {"axis": "x", "value": x * 32767 // 1919}}, {"type": "abs", "data": {"axis": "y", "value": y * 32767 // 1079}}]); time.sleep(0.3)
        send([{"type": "btn", "data": {"down": True, "button": "left"}}]); time.sleep(0.15)
        send([{"type": "btn", "data": {"down": False, "button": "left"}}]); time.sleep(0.4)
        print(f"click {x},{y}", flush=True)
    elif kind == "shot":
        q({"execute": "screendump", "arguments": {"filename": os.path.join(W, arg + ".png"), "format": "png"}}); time.sleep(0.5)
        print(f"shot {arg}.png", flush=True)
    elif kind == "grep":
        for l in lines[mark:]:
            if arg in l: print("   | " + l[:220], flush=True)
        mark = len(lines); continue
p.terminate()
try: p.wait(timeout=10)
except Exception: p.kill()
time.sleep(0.5); logf.close(); os.unlink(img)
crash = [l for l in lines if any(x in l for x in ("KERNEL PANIC", "panicked at", "DOUBLE FAULT", "GENERAL PROTECTION FAULT", "[idt] PAGE FAULT"))]
print(f"serial lines: {len(lines)}; fault lines: {len(crash)}", flush=True)
for l in crash[:3]: print("   ! " + l[:200])
