#!/usr/bin/env python3
"""Boot the image, type a sequence of commands via QMP send-key, and screenshot.
Proves the interactive shell headless (no GUI, no KVM)."""
import json, os, socket, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
OUT = sys.argv[2] if len(sys.argv) > 2 else "shell.png"
WAIT = int(sys.argv[3]) if len(sys.argv) > 3 else 27
SCRIPT = sys.argv[4] if len(sys.argv) > 4 else (
    "help\n"
    "cat /etc/hostname\n"
    "write /etc/motd welkom-bij-euroos\n"
    "ls /etc\n"
    "cat /etc/motd\n"
    "df\n"
)
OVMF = "/usr/share/ovmf/OVMF.fd"
QMP = "/tmp/ek-qmp-typed.sock"

QMAP = {" ": "spc", "\n": "ret", "/": "slash", ".": "dot", "-": "minus", "_": "shift-minus",
        ">": "shift-dot", "<": "shift-comma", "|": "shift-backslash", ",": "comma"}
for c in "abcdefghijklmnopqrstuvwxyz":
    QMAP[c] = c
for c in "0123456789":
    QMAP[c] = c

try:
    os.unlink(QMP)
except FileNotFoundError:
    pass

qemu = subprocess.Popen([
    "qemu-system-x86_64", "-machine", "q35", "-m", "256M",
    "-bios", OVMF, "-drive", f"format=raw,file={IMG}",
    "-display", "none", "-serial", "file:serial.log",
    "-qmp", f"unix:{QMP},server,nowait", "-netdev", "user,id=n0,ipv4=on,ipv6=on", "-device", "virtio-net-pci,netdev=n0,disable-modern=on",
    "-object", "filter-dump,id=d0,netdev=n0,file=net.pcap",
    "-no-reboot",
], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

for _ in range(50):
    if os.path.exists(QMP):
        break
    time.sleep(0.2)

s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(QMP)
f = s.makefile("rwb", buffering=0)

def cmd(obj):
    f.write((json.dumps(obj) + "\n").encode())
    return json.loads(f.readline().decode())

f.readline()
cmd({"execute": "qmp_capabilities"})

print(f"[typed] boot, waiting {WAIT}s...", flush=True)
time.sleep(WAIT)

def send_key(qcodes):
    keys = []
    for q in qcodes.split("-"):
        keys.append({"type": "qcode", "data": q})
    cmd({"execute": "send-key", "arguments": {"keys": keys}})

print(f"[typed] typing: {SCRIPT!r}", flush=True)
for ch in SCRIPT:
    q = QMAP.get(ch)
    if not q:
        continue
    send_key(q)
    time.sleep(0.07 if ch != "\n" else 0.5)

time.sleep(2)
r = cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(OUT), "format": "png"}})
print("[typed] screendump:", "ok" if "error" not in r else r, flush=True)
cmd({"execute": "quit"})
time.sleep(1)
qemu.terminate()
try:
    qemu.wait(timeout=5)
except Exception:
    qemu.kill()
