#!/usr/bin/env python3
"""Boot, beweeg de PS/2-muis via QMP en (optioneel) sleep een venster, screenshot."""
import json, os, socket, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
OUT = sys.argv[2] if len(sys.argv) > 2 else "mouse.png"
WAIT = int(sys.argv[3]) if len(sys.argv) > 3 else 36
MODE = sys.argv[4] if len(sys.argv) > 4 else "move"  # "move" of "drag"
OVMF = "/usr/share/ovmf/OVMF.fd"
QMP = "/tmp/ek-mouse.sock"

try: os.unlink(QMP)
except FileNotFoundError: pass

qemu = subprocess.Popen([
    "qemu-system-x86_64", "-machine", "q35", "-m", "256M", "-bios", OVMF,
    "-drive", f"format=raw,file={IMG}", "-display", "none",
    "-serial", "file:serial.log", "-qmp", f"unix:{QMP},server,nowait", "-no-reboot",
], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

for _ in range(50):
    if os.path.exists(QMP): break
    time.sleep(0.2)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(QMP)
f = s.makefile("rwb", buffering=0)
def cmd(o): f.write((json.dumps(o)+"\n").encode()); return json.loads(f.readline().decode())
f.readline(); cmd({"execute": "qmp_capabilities"})

print(f"[mouse] boot, wacht {WAIT}s...", flush=True)
time.sleep(WAIT)

def move(dx, dy, steps=20):
    for _ in range(steps):
        cmd({"execute": "input-send-event", "arguments": {"events": [
            {"type": "rel", "data": {"axis": "x", "value": dx}},
            {"type": "rel", "data": {"axis": "y", "value": dy}},
        ]}})
        time.sleep(0.02)

def button(down):
    cmd({"execute": "input-send-event", "arguments": {"events": [
        {"type": "btn", "data": {"button": "left", "down": down}}]}})

# Cursor start in het midden (~960,540). Beweeg naar de Systeem-titelbalk
# (rechtsboven, ~x 980, y 166): naar rechts + omhoog.
move(+4, -18, 22)   # omhoog-rechts naar de titelbalk-zone

if MODE == "drag":
    time.sleep(0.3)
    button(True)
    time.sleep(0.2)
    move(-8, +14, 20)   # sleep het venster naar links-onder
    time.sleep(0.2)
    button(False)

time.sleep(1.5)
r = cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(OUT), "format": "png"}})
print("[mouse] screendump:", "ok" if "error" not in r else r, flush=True)
cmd({"execute": "quit"}); time.sleep(1)
try: qemu.wait(timeout=5)
except Exception: qemu.kill()
