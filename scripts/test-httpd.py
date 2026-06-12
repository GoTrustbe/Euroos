#!/usr/bin/env python3
"""Test de ACHTERGROND-HTTP-server: boot met hostfwd (host :5581 -> gast :80) +
QMP. Typ `httpd` om de server aan te zetten, doe daarna MEERDERE HTTP-verzoeken
vanaf de host terwijl de desktop interactief blijft. Elk verzoek moet bediend
worden door net::service() in de desktop-lus. Screenshot toont de teller.
"""
import json, os, socket, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
OUT = sys.argv[2] if len(sys.argv) > 2 else "httpd.png"
WAIT = int(sys.argv[3]) if len(sys.argv) > 3 else 30
HOSTPORT = 5581
OVMF = "/usr/share/ovmf/OVMF.fd"
QMP = "/tmp/ek-qmp-httpd.sock"
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
    "-display", "none", "-serial", "file:serial-httpd.log",
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
print(f"[httpd] boot, wacht {WAIT}s...", flush=True)
time.sleep(WAIT)

def send_key(qcodes):
    cmd({"execute": "send-key", "arguments": {"keys": [{"type": "qcode", "data": q} for q in qcodes.split("-")]}})

print("[httpd] typt: 'httpd'", flush=True)
for ch in "httpd\n":
    send_key(QMAP[ch])
    time.sleep(0.08 if ch != "\n" else 0.5)
time.sleep(1)

# Doe meerdere HTTP-verzoeken; elk moet bediend worden door de desktop-lus.
ok = 0
req = b"GET / HTTP/1.0\r\nHost: euroos\r\n\r\n"
for i in range(5):
    for _ in range(20):  # retry tot bediend (server pollt per desktop-tick)
        try:
            c = socket.create_connection(("127.0.0.1", HOSTPORT), timeout=2)
            c.sendall(req)
            c.settimeout(4)
            data = b""
            while True:
                chunk = c.recv(2048)
                if not chunk:
                    break
                data += chunk
            c.close()
            if b"EuroOS" in data:
                ok += 1
                print(f"[httpd] verzoek {i+1}: bediend ({len(data)} bytes)", flush=True)
                break
        except (ConnectionRefusedError, ConnectionResetError, socket.timeout, OSError):
            pass
        time.sleep(0.3)

time.sleep(2)
r = cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(OUT), "format": "png"}})
print("[httpd] screendump:", "ok" if "error" not in r else r, flush=True)
cmd({"execute": "quit"})
time.sleep(1)
qemu.terminate()
try:
    qemu.wait(timeout=5)
except Exception:
    qemu.kill()

print(f"\n[httpd] {ok}/5 verzoeken bediend door de achtergrond-server.", flush=True)
sys.exit(0 if ok >= 3 else 1)
