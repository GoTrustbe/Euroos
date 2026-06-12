#!/usr/bin/env python3
"""Boot de EuroKernel-image in QEMU (headless) en maak via QMP een screenshot.
Geen KVM nodig: pure TCG-emulatie. Gebruikt om de boot zonder GUI te bewijzen.
"""
import json, os, socket, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
OUT = sys.argv[2] if len(sys.argv) > 2 else "boot.png"
# Render is zwaarder bij 1920x1080 op TCG; standaard ruimer wachten.
WAIT = int(sys.argv[3]) if len(sys.argv) > 3 else int(os.environ.get("WAIT", "65"))
OVMF = "/usr/share/ovmf/OVMF.fd"
QMP = "/tmp/ek-qmp.sock"

for p in (QMP,):
    try: os.unlink(p)
    except FileNotFoundError: pass

qemu = subprocess.Popen([
    "qemu-system-x86_64",
    "-machine", "q35",
    # TCG-CPU met SMEP/SMAP zodat de kernel-bescherming (CR4 + syscall-AC-venster)
    # ook in de screenshot-boot wordt afgedwongen — qemu64 adverteert ze anders niet.
    "-cpu", "qemu64,+smep,+smap",
    # Cores: standaard 1 (snel op TCG). Zet EK_SMP=N voor multi-core (ACPI/MADT-test).
    "-smp", os.environ.get("EK_SMP", "1"),
    "-m", "256M",
    "-bios", OVMF,
    "-drive", f"format=raw,file={IMG}",
    "-display", "none",
    "-serial", "file:serial.log",
    "-qmp", f"unix:{QMP},server,nowait",
    "-netdev", "user,id=n0,ipv4=on,ipv6=on", "-device", "virtio-net-pci,netdev=n0,disable-modern=on",
    "-object", "filter-dump,id=d0,netdev=n0,file=net.pcap",
    "-no-reboot",
], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

# Wacht tot het QMP-socket bestaat.
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

f.readline()  # QMP greeting
cmd({"execute": "qmp_capabilities"})

print(f"[screenshot] booting, wacht {WAIT}s op render (TCG is traag)...", flush=True)
time.sleep(WAIT)

# Probeer PNG; val terug op PPM als deze QEMU geen PNG-screendump heeft.
r = cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(OUT), "format": "png"}})
if "error" in r:
    ppm = os.path.abspath(OUT.rsplit(".", 1)[0] + ".ppm")
    r2 = cmd({"execute": "screendump", "arguments": {"filename": ppm}})
    print("[screenshot] PNG niet ondersteund, PPM geschreven:", r2, flush=True)
else:
    print("[screenshot] PNG geschreven:", OUT, flush=True)

cmd({"execute": "quit"})
time.sleep(1)
qemu.terminate()
try: qemu.wait(timeout=5)
except Exception: qemu.kill()
print("[screenshot] klaar.", flush=True)
