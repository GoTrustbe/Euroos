#!/usr/bin/env python3
"""End-to-end interactivity test (completion sprint): boot with a USB keyboard,
wait until the desktop is running, TYPE a shell command + Enter via QMP, and verify
that the whole loop works — USB key → xHCI interrupt-IN → scancode ring → poll_key →
shell prompt → Enter → exec → output (teed to serial as `[e2e]` lines).

Proves at the same time that HLT-idle does not break input responsiveness (the desktop
sleeps between frames but is woken by the keyboard IRQ)."""
import json
import os
import socket
import subprocess
import sys
import time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
LOG = "serial-e2e.log"
QMP = "/tmp/ek-qmp-e2e.sock"
# Reuse an ALREADY-FORMATTED disk (otherwise the first-boot format+
# populate costs hundreds of TCG seconds before the desktop runs).
D1 = os.environ.get("DISK", "/tmp/hdadisk.img")
WAIT = int(os.environ.get("WAIT", "260"))
# The command we "type" (USB-HID qcodes). Default: `lsdev` (EuroDevice tree).
CMD = os.environ.get("CMD", "lsdev")

for p in (LOG, QMP):
    try:
        os.remove(p)
    except FileNotFoundError:
        pass
if not os.path.exists(D1):
    subprocess.run(["qemu-img", "create", "-f", "raw", D1, "64M"], check=True, stdout=subprocess.DEVNULL)

OVMF = next((c for c in ("/usr/share/ovmf/OVMF.fd", "/usr/share/OVMF/OVMF.fd") if os.path.exists(c)), None)
assert OVMF, "OVMF not found"

qemu = subprocess.Popen([
    "qemu-system-x86_64", "-machine", "q35", "-m", "256M", "-cpu", "qemu64,+smep,+smap",
    "-bios", OVMF,
    "-drive", f"format=raw,file={IMG}",
    "-drive", f"format=raw,file={D1},if=none,id=d1",
    "-device", "virtio-blk-pci,drive=d1,disable-modern=on",
    "-device", "qemu-xhci,id=xhci", "-device", "usb-kbd,bus=xhci.0",
    "-display", "none", "-serial", f"file:{LOG}",
    "-qmp", f"unix:{QMP},server,nowait", "-no-reboot",
])

def qmp_connect(path, tries=80):
    for _ in range(tries):
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(path)
            return s
        except OSError:
            time.sleep(0.5)
    return None

def send(sock, cmd):
    sock.sendall((json.dumps(cmd) + "\n").encode())
    time.sleep(0.15)
    try:
        sock.recv(65536)
    except OSError:
        pass

# Map ordinary characters → QEMU qcode names.
QCODE = {**{c: c for c in "abcdefghijklmnopqrstuvwxyz0123456789"}, " ": "spc", "/": "slash", "-": "minus", ".": "dot"}

sock = qmp_connect(QMP)
assert sock, "QMP not reachable"
sock.recv(65536)
send(sock, {"execute": "qmp_capabilities"})

print(f"[e2e] booting, waiting until the desktop runs (<={WAIT}s)...", flush=True)
deadline = time.time() + WAIT
ready = False
while time.time() < deadline:
    time.sleep(3)
    if os.path.exists(LOG):
        t = open(LOG, errors="replace").read()
        # Wait explicitly until the INTERACTIVE desktop loop is running (and thus xhci::poll
        # is actively harvesting input) — otherwise early keys are dropped.
        if "interactieve loop gestart" in t:
            ready = True
            break
print(f"[e2e] desktop ready={ready}; now typing '{CMD}' + Enter via the USB keyboard...", flush=True)
time.sleep(2)

for ch in CMD:
    qc = QCODE.get(ch)
    if qc:
        send(sock, {"execute": "send-key", "arguments": {"keys": [{"type": "qcode", "data": qc}]}})
        time.sleep(0.35)
# Enter.
send(sock, {"execute": "send-key", "arguments": {"keys": [{"type": "qcode", "data": "ret"}]}})

# Give the desktop loop time to process the keys + execute (HLT-idle wakes
# on every key IRQ, but TCG is slow).
time.sleep(20)
qemu.terminate()
try:
    qemu.wait(timeout=10)
except subprocess.TimeoutExpired:
    qemu.kill()

print("\n===== [e2e] lines from serial =====", flush=True)
got_cmd = got_output = False
if os.path.exists(LOG):
    for line in open(LOG, errors="replace"):
        if "[e2e]" in line:
            print(line.rstrip())
            if f"$ {CMD}" in line:
                got_cmd = True
            elif "[e2e]" in line and "$" not in line:
                got_output = True
print(f"\n[e2e] RESULT: command-echo={got_cmd}, output-received={got_output} -> "
      + ("PASSED ✓" if got_cmd and got_output else "FAILED"))
