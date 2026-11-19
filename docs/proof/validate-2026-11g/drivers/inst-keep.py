#!/usr/bin/env python3
"""Phase 0 of rename-persist.py alone: the public image installs itself on a blank
4 GiB virtio disk, driven by the wizard key sequence. Prints one verdict line.
usage: inst.py IMAGE WORKDIR PINS"""
import json, os, socket, subprocess, sys, threading, time, shutil
IMG, W, PIN = sys.argv[1], os.path.abspath(sys.argv[2]), sys.argv[3]
os.makedirs(W, exist_ok=True)
HOST, USER, PASSWORD = "euro-test", "tester", "Sovereign-Pass-2026!"
OVMF = "/usr/share/ovmf/OVMF.fd"
QMAP = {" ": "spc", "\n": "ret", "-": "minus", "_": "shift-minus", ".": "dot", "!": "shift-1",
        "@": "shift-2", "#": "shift-3", "$": "shift-4", "%": "shift-5", "/": "slash", ",": "comma"}
for c in "abcdefghijklmnopqrstuvwxyz0123456789": QMAP[c] = c
for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ": QMAP[c] = "shift-" + c.lower()
live = os.path.join(W, "live.img"); shutil.copyfile(IMG, live)
TARGET = os.path.join(W, "target.img")
subprocess.run(["qemu-img", "create", "-f", "raw", TARGET, "4G"], check=True, stdout=subprocess.DEVNULL)
qmp = os.path.join(W, "qmp")
if os.path.exists(qmp): os.unlink(qmp)
lines = []; logf = open(os.path.join(W, "serial.log"), "w")
cmd = ["taskset", "-c", PIN, "qemu-system-x86_64", "-machine", "q35", "-enable-kvm", "-cpu", "host", "-m", "2048M", "-smp", "2",
       "-bios", OVMF, "-display", "none", "-vga", "std", "-serial", "stdio", "-no-reboot", "-nic", "none",
       "-qmp", f"unix:{qmp},server,nowait", "-drive", f"format=raw,file={live}",
       "-drive", f"id=tgt,format=raw,file={TARGET},if=none" + (",readonly=on" if os.environ.get("RO") == "1" else ""), "-device", "virtio-blk-pci,drive=tgt,disable-modern=on"]
p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
def rd():
    for raw in p.stdout:
        l = raw.decode("utf-8", "replace").rstrip("\r\n"); lines.append(l); logf.write(l + "\n"); logf.flush()
threading.Thread(target=rd, daemon=True).start()
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
def key(k):
    q({"execute": "send-key", "arguments": {"keys": [{"type": "qcode", "data": x} for x in k.split("-")]}}); time.sleep(0.12)
def wait(needle, t, after=0):
    end = time.time() + t
    while time.time() < end:
        if any(needle in l for l in lines[after:]): return True
        if p.poll() is not None: break
        time.sleep(0.2)
    return any(needle in l for l in lines[after:])
up = wait("[desktop] interactive loop started", 600)
time.sleep(3)
key("ret"); time.sleep(1.5); key("ret"); time.sleep(1.5)
key("up"); key("ret"); time.sleep(2); key("ret"); time.sleep(1.5)
for ch in HOST + "\n" + USER + "\n" + PASSWORD + "\n" + PASSWORD: key(QMAP[ch])
time.sleep(1); key("ret"); time.sleep(2); key("ret"); time.sleep(2)
m0 = len(lines); t0 = time.time(); key("ret")
done = wait("[q1x3]", 900, after=m0)
txt = "\n".join(lines[m0:])
ok = up and done and "→ OK" in txt and wait("[wizard] Installation complete", 60, after=m0)
v = [l for l in lines[m0:] if l.startswith("[q1x3]")]
print(f"[inst] {'OK' if ok else 'FAILED'} up={up} done={done} install {time.time()-t0:.0f}s :: {v[-1][:260] if v else '-'}", flush=True)
time.sleep(2)
try:
    q({"execute": "quit"})
except Exception: pass
try: p.wait(timeout=20)
except Exception:
    if p.pid and p.pid > 1: p.kill()
os.unlink(live)
