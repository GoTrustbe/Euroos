#!/usr/bin/env python3
"""Soak run (night plan N3.3): hours of desktop use in QEMU, one verdict.

Boots the dev image with networking, waits for the desktop, then for DURATION
seconds drives the Terminal over USB HID (QMP): a rotating list of commands
(memory report, file listing, the native and the WASM example app opened and
closed with Escape, the package manager, the app store list). Every command
must be echoed by the shell within RESPONSE_S seconds (responsiveness); the
`free` report is parsed each round (memory drift); the serial log must never
show a kernel panic or fault. Hourly screenshots. Verdict + JSON at the end.

Usage (as root): DURATION=7200 python3 scripts/soak.py [outdir]
"""
import json, os, socket, subprocess, sys, time

OUT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/euroos-soak"
os.makedirs(OUT, exist_ok=True)
LOG = os.path.join(OUT, "serial.log"); QMP = os.path.join(OUT, "qmp.sock")
DURATION = int(os.environ.get("DURATION", "7200"))
INTERVAL = int(os.environ.get("INTERVAL", "45"))
RESPONSE_S = int(os.environ.get("RESPONSE_S", "40"))

AZ = {c: c for c in "bcdefghijklnoprstuvxy"}
AZ.update({"a": "q", "q": "a", "z": "w", "w": "z", "m": "semicolon", " ": "spc", "\n": "ret",
           "-": "equal", ":": "dot", "/": "shift-dot", ".": "shift-comma", "_": "shift-equal"})
for c in "1234567890":
    AZ[c] = "shift-" + c

try: os.unlink(QMP)
except FileNotFoundError: pass
p = subprocess.Popen(["qemu-system-x86_64", "-machine", "q35", "-cpu", "qemu64,+smep,+smap", "-m", "3072M",
    "-bios", "/usr/share/ovmf/OVMF.fd", "-display", "none", "-serial", f"file:{LOG}",
    "-qmp", f"unix:{QMP},server,nowait", "-no-reboot", "-drive", "format=raw,file=eurokernel.img",
    "-device", "qemu-xhci,id=xhci", "-device", "usb-kbd", "-device", "usb-tablet",
    "-netdev", "user,id=n0", "-device", "virtio-net-pci,netdev=n0"],
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
    send([{"type": "abs", "data": {"axis": "x", "value": x * 32767 // (sw - 1)}},
          {"type": "abs", "data": {"axis": "y", "value": y * 32767 // (sh - 1)}}]); time.sleep(0.3)
    send([{"type": "btn", "data": {"down": True, "button": "left"}}]); time.sleep(0.15)
    send([{"type": "btn", "data": {"down": False, "button": "left"}}]); time.sleep(0.5)
def shot(name):
    cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(os.path.join(OUT, name)), "format": "png"}}); time.sleep(2)
def type_cmd(c):
    for ch in c + "\n": key(AZ[ch])

t0 = time.time(); up = False
while time.time() - t0 < 1500:
    if "[desktop] interactive loop started" in log(): up = True; break
    if "KERNEL PANIC" in log() or p.poll() is not None: break
    time.sleep(5)
print(f"desktop={up} at {int(time.time()-t0)}s", flush=True)
result = {"desktop": up, "rounds": 0, "missed_echo": [], "free_kib": [], "panic": False, "duration_s": DURATION}
if up:
    time.sleep(15)
    click(1100, 171)  # the Terminal title bar: keyboard focus
    time.sleep(1)
    cmds = ["free", "ls /", "app run hello-euro", "eupkg list", "app run hello-euro-wasm", "app store", "mem", "ls /home/euro"]
    start = time.time(); last_shot = start; i = 0
    while time.time() - start < DURATION:
        c = cmds[i % len(cmds)]
        before = len(log())
        type_cmd(c)
        deadline = time.time() + RESPONSE_S; echoed = False
        while time.time() < deadline:
            txt = log()[before:]
            if f"[e2e] $ {c}" in txt: echoed = True; break
            time.sleep(1)
        if not echoed:
            result["missed_echo"].append({"round": i, "cmd": c, "t": int(time.time() - start)})
            print(f"round {i}: NO ECHO for {c!r}", flush=True)
        if c.startswith("app run"):
            time.sleep(12)
            key("esc")  # the example apps quit on Escape (the Terminal regains focus)
            time.sleep(3)
            click(1100, 171)
        if c == "free":
            txt = log()[before:]
            for line in txt.splitlines():
                if line.startswith("[e2e] Mem:"):
                    parts = line.split()
                    try: result["free_kib"].append({"round": i, "used": int(parts[3]), "free": int(parts[4])})
                    except (IndexError, ValueError): pass
        if "KERNEL PANIC" in log() or "GENERAL PROTECTION FAULT" in log() or p.poll() is not None:
            result["panic"] = True; print("KERNEL FAULT", flush=True); break
        if time.time() - last_shot > 3600:
            shot(f"hour-{int((time.time()-start)//3600)}.png"); last_shot = time.time()
        i += 1; result["rounds"] = i
        time.sleep(max(0, INTERVAL - 2))
    shot("final.png")
try: cmd({"execute": "quit"})
except Exception: pass
p.wait(timeout=30)
drift = None
if len(result["free_kib"]) >= 3:
    a, b = result["free_kib"][1]["used"], result["free_kib"][-1]["used"]
    drift = (b - a) / max(a, 1)
result["used_drift"] = drift
ok = up and not result["panic"] and not result["missed_echo"] and (drift is None or drift < 0.10)
result["verdict"] = "PASS" if ok else "FAIL"
json.dump(result, open(os.path.join(OUT, "soak.json"), "w"), indent=1)
print(f"SOAK: {result['verdict']}  rounds={result['rounds']} missed={len(result['missed_echo'])} panic={result['panic']} used-drift={drift}", flush=True)
sys.exit(0 if ok else 1)
