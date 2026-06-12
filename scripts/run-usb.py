#!/usr/bin/env python3
"""Boot EuroOS met een qemu-xhci-USB-controller + een USB-toetsenbord en -muis
(I1-harness). Bewijst de echte xHCI-driver: controller-reset, slot-enable,
address-device, descriptor-lezen, configure-endpoint en de interrupt-IN-poll.

Na de boot injecteren we via QMP `sendkey`/`input-send-event` een paar toetsen +
muisbeweging, zodat de interrupt-IN-pad (de [xhci-rpt]-rapporten) zichtbaar
geverifieerd wordt. Headless; serial → serial-usb.log."""
import json
import os
import socket
import subprocess
import sys
import time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
LOG = "serial-usb.log"
QMP = "/tmp/ek-qmp-usb.sock"
D1 = "/tmp/usb-disk1.img"
WAIT = int(os.environ.get("WAIT", "240"))

for p in (LOG, QMP):
    try:
        os.remove(p)
    except FileNotFoundError:
        pass
if not os.path.exists(D1):
    subprocess.run(["qemu-img", "create", "-f", "raw", D1, "64M"], check=True, stdout=subprocess.DEVNULL)

OVMF = None
for c in ("/usr/share/ovmf/OVMF.fd", "/usr/share/OVMF/OVMF.fd", "/usr/share/edk2-ovmf/x64/OVMF.fd"):
    if os.path.exists(c):
        OVMF = c
        break
assert OVMF, "OVMF niet gevonden"

qemu = subprocess.Popen([
    "qemu-system-x86_64", "-machine", "q35", "-m", "256M",
    "-cpu", "qemu64,+smep,+smap",
    "-bios", OVMF,
    "-drive", f"format=raw,file={IMG}",
    "-drive", f"format=raw,file={D1},if=none,id=d1",
    "-device", "virtio-blk-pci,drive=d1,disable-modern=on",
    # De USB-stack onder test: een xHCI-controller + HID-boot-toetsenbord + -muis.
    "-device", "qemu-xhci,id=xhci",
    "-device", "usb-kbd,bus=xhci.0",
    "-device", "usb-mouse,bus=xhci.0",
    "-display", "none", "-serial", f"file:{LOG}",
    "-qmp", f"unix:{QMP},server,nowait",
    "-no-reboot",
])

def qmp_connect(path, tries=60):
    for _ in range(tries):
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(path)
            return s
        except OSError:
            time.sleep(0.5)
    return None

def qmp(sock, cmd):
    sock.sendall((json.dumps(cmd) + "\n").encode())
    time.sleep(0.2)
    try:
        return sock.recv(65536).decode(errors="replace")
    except OSError:
        return ""

print(f"[usb] boot, wacht {WAIT}s op desktop...", flush=True)
sock = qmp_connect(QMP)
if sock:
    buf = sock.recv(65536)  # greeting
    qmp(sock, {"execute": "qmp_capabilities"})

# Wacht tot de xHCI-enumeratie in de log staat (of timeout).
deadline = time.time() + WAIT
enumerated = False
while time.time() < deadline:
    time.sleep(3)
    if os.path.exists(LOG):
        with open(LOG, errors="replace") as f:
            t = f.read()
        if "enumeratie klaar" in t:
            enumerated = True
            break

print(f"[usb] enumeratie gedetecteerd: {enumerated}; injecteer nu toetsen + muis...", flush=True)
if sock:
    # Toetsenbord: typ 'e','u','r','o' (qemu sendkey gebruikt qcode-namen).
    for key in ("e", "u", "r", "o"):
        qmp(sock, {"execute": "send-key", "arguments": {"keys": [{"type": "qcode", "data": key}]}})
        time.sleep(0.4)
    # Muis: relatieve beweging + linkerklik via input-send-event.
    for _ in range(3):
        qmp(sock, {"execute": "input-send-event", "arguments": {"events": [
            {"type": "rel", "data": {"axis": "x", "value": 20}},
            {"type": "rel", "data": {"axis": "y", "value": 10}},
        ]}})
        time.sleep(0.3)
    qmp(sock, {"execute": "input-send-event", "arguments": {"events": [
        {"type": "btn", "data": {"button": "left", "down": True}}]}})
    time.sleep(0.2)
    qmp(sock, {"execute": "input-send-event", "arguments": {"events": [
        {"type": "btn", "data": {"button": "left", "down": False}}]}})

# Geef de kernel-poll-loop even tijd om de interrupt-transfers te harvesten.
time.sleep(8)
qemu.terminate()
try:
    qemu.wait(timeout=10)
except subprocess.TimeoutExpired:
    qemu.kill()

print("\n===== [xhci]-regels uit de serial-log =====", flush=True)
if os.path.exists(LOG):
    with open(LOG, errors="replace") as f:
        for line in f:
            if "xhci" in line or "[euro] xHCI" in line:
                print(line.rstrip())
