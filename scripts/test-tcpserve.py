#!/usr/bin/env python3
"""Test the POSIX server sockets (C2): boot with hostfwd (host :5582 -> guest :8080),
type `tcpserve` in the terminal (listens on :8080 via listen()/accept()), then connect
from the host and check whether the server socket accepted the connection and
replied. Proves the passive-open chain end-to-end with a real client.
"""
import json, os, socket, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
OUT = sys.argv[2] if len(sys.argv) > 2 else "tcpserve.png"
WAIT = int(sys.argv[3]) if len(sys.argv) > 3 else 140
HOSTPORT = 5582
OVMF = "/usr/share/ovmf/OVMF.fd"
QMP = "/tmp/ek-qmp-tcpserve.sock"
QMAP = {" ": "spc", "\n": "ret"}
for c in "abcdefghijklmnopqrstuvwxyz":
    QMAP[c] = c

try:
    os.unlink(QMP)
except FileNotFoundError:
    pass

qemu = subprocess.Popen([
    "qemu-system-x86_64", "-machine", "q35", "-m", "256M",
    "-cpu", "qemu64,+smep,+smap",
    "-bios", OVMF, "-drive", f"format=raw,file={IMG}",
    "-display", "none", "-serial", "file:serial-tcpserve.log",
    "-qmp", f"unix:{QMP},server,nowait",
    "-netdev", f"user,id=n0,ipv4=on,ipv6=on,hostfwd=tcp::{HOSTPORT}-:8080",
    "-device", "virtio-net-pci,netdev=n0,disable-modern=on",
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
print(f"[tcpserve] boot, wait {WAIT}s...", flush=True)
time.sleep(WAIT)

def send_key(qcodes):
    cmd({"execute": "send-key", "arguments": {"keys": [{"type": "qcode", "data": q} for q in qcodes.split("-")]}})

print("[tcpserve] types: 'tcpserve'", flush=True)
for ch in "tcpserve\n":
    send_key(QMAP[ch])
    time.sleep(0.08 if ch != "\n" else 0.5)

# Give the shell a moment to call listen(), then connect from the host. The accept()
# in the guest blocks (~6 s guest time); under TCG that is plenty of wall-clock.
ok = False
got = b""
req = b"GET / HTTP/1.0\r\nHost: euroos\r\n\r\n"
for attempt in range(40):
    try:
        c = socket.create_connection(("127.0.0.1", HOSTPORT), timeout=2)
        c.sendall(req)
        c.settimeout(4)
        while True:
            chunk = c.recv(2048)
            if not chunk:
                break
            got += chunk
        c.close()
        if b"accept() werkt" in got or b"EuroOS" in got:
            ok = True
            print(f"[tcpserve] server socket replied ({len(got)} bytes)", flush=True)
            break
    except (ConnectionRefusedError, ConnectionResetError, socket.timeout, OSError):
        pass
    time.sleep(0.5)

time.sleep(2)
cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(OUT), "format": "png"}})
cmd({"execute": "quit"})
time.sleep(1)
qemu.terminate()
try:
    qemu.wait(timeout=5)
except Exception:
    qemu.kill()

print(f"[tcpserve] result: {'PASS' if ok else 'FAIL'} ({len(got)} bytes received)", flush=True)
sys.exit(0 if ok else 1)
