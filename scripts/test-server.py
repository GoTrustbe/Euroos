#!/usr/bin/env python3
"""Test EuroOS' own HTTP SERVER end-to-end.

Boot the image with a hostfwd (host :5580 -> guest :80) and a QMP socket. Wait
for the boot, type `serve` in the interactive shell (EuroOS then starts listening on :80
via net::tcp_serve_once), and connect immediately afterwards from the host. EuroNet's own
TCP stack completes the handshake and serves our page. We check the
response and take a screenshot of the desktop ('client served').
"""
import json, os, socket, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
OUT = sys.argv[2] if len(sys.argv) > 2 else "server.png"
WAIT = int(sys.argv[3]) if len(sys.argv) > 3 else 27
HOSTPORT = 5580
OVMF = "/usr/share/ovmf/OVMF.fd"
QMP = "/tmp/ek-qmp-server.sock"

QMAP = {" ": "spc", "\n": "ret"}
for c in "abcdefghijklmnopqrstuvwxyz":
    QMAP[c] = c

try:
    os.unlink(QMP)
except FileNotFoundError:
    pass

qemu = subprocess.Popen([
    "qemu-system-x86_64", "-machine", "q35", "-m", "256M",
    "-bios", OVMF, "-drive", f"format=raw,file={IMG}",
    "-display", "none", "-serial", "file:serial-server.log",
    "-qmp", f"unix:{QMP},server,nowait",
    "-netdev", f"user,id=n0,ipv4=on,ipv6=on,hostfwd=tcp::{HOSTPORT}-:80",
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

print(f"[server] booting, waiting {WAIT}s...", flush=True)
time.sleep(WAIT)

def send_key(qcodes):
    keys = [{"type": "qcode", "data": q} for q in qcodes.split("-")]
    cmd({"execute": "send-key", "arguments": {"keys": keys}})

print("[server] typing: 'serve'", flush=True)
for ch in "serve\n":
    send_key(QMAP[ch])
    time.sleep(0.08 if ch != "\n" else 0.5)

# EuroOS is now listening on :80. Connect from the host (with retry).
print(f"[server] connecting to localhost:{HOSTPORT} ...", flush=True)
request = b"GET / HTTP/1.0\r\nHost: euroos.local\r\n\r\n"
response = None
for attempt in range(40):
    try:
        c = socket.create_connection(("127.0.0.1", HOSTPORT), timeout=2)
        c.sendall(request)
        c.settimeout(4)
        data = b""
        while True:
            chunk = c.recv(2048)
            if not chunk:
                break
            data += chunk
        c.close()
        if b"EuroOS" in data:
            response = data
            break
    except (ConnectionRefusedError, ConnectionResetError, socket.timeout, OSError):
        pass
    time.sleep(0.4)

time.sleep(2)
r = cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(OUT), "format": "png"}})
print("[server] screendump:", "ok" if "error" not in r else r, flush=True)
cmd({"execute": "quit"})
time.sleep(1)
qemu.terminate()
try:
    qemu.wait(timeout=5)
except Exception:
    qemu.kill()

if response:
    print("\n[server] RESPONSE from EuroOS' own HTTP server:\n", flush=True)
    print(response.decode("utf-8", "replace"))
    print(f"\n[server] OK — {len(response)} bytes served by EuroNet.", flush=True)
    sys.exit(0)
else:
    print("[server] FAIL — no response received.", flush=True)
    sys.exit(1)
