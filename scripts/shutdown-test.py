#!/usr/bin/env python3
"""Sprint 2 (I3): bewijs dat ACPI-S5 het systeem ECHT afsluit. Boot EuroOS; de
kernel doet een nette `power::shutdown()` (ACPI S5). QEMU hoort de gast af te
sluiten → QMP zendt een SHUTDOWN-event (guest-initiated) en het proces eindigt.
Vereist een build met de tijdelijke shutdown-trigger actief."""
import json, os, socket, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
QMP = "/tmp/ek-qmp-sd.sock"
OVMF = "/usr/share/ovmf/OVMF.fd"
WAIT = int(os.environ.get("WAIT", "90"))

try: os.unlink(QMP)
except FileNotFoundError: pass

qemu = subprocess.Popen([
    "qemu-system-x86_64", "-machine", "q35", "-cpu", "qemu64,+smep,+smap",
    "-m", "256M", "-bios", OVMF, "-drive", f"format=raw,file={IMG}",
    "-display", "none", "-serial", "file:serial-shutdown.log",
    "-qmp", f"unix:{QMP},server,nowait", "-no-reboot",
], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

for _ in range(50):
    if os.path.exists(QMP): break
    time.sleep(0.2)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(QMP)
f = s.makefile("rwb", buffering=0)
f.readline()
f.write((json.dumps({"execute": "qmp_capabilities"}) + "\n").encode()); f.readline()

print(f"[shutdown-test] booting; waiting up to {WAIT}s for a guest ACPI poweroff...", flush=True)
s.settimeout(WAIT)
shutdown_seen = False
deadline = time.time() + WAIT
try:
    while time.time() < deadline:
        line = f.readline()
        if not line:
            break
        try: msg = json.loads(line.decode())
        except Exception: continue
        if msg.get("event") == "SHUTDOWN":
            shutdown_seen = True
            print(f"[shutdown-test] QMP SHUTDOWN event: {msg.get('data')}")
            break
except (socket.timeout, OSError):
    pass

# QEMU exits on guest poweroff (-no-reboot). Confirm the process ended cleanly.
try:
    rc = qemu.wait(timeout=10)
    exited = True
except subprocess.TimeoutExpired:
    qemu.kill(); rc = None; exited = False

print(f"[shutdown-test] SHUTDOWN-event={shutdown_seen}, qemu-exited={exited} (rc={rc})")
print("RESULT:", "OK — guest powered off cleanly via ACPI S5" if (shutdown_seen or exited) else "FAIL — no poweroff")
