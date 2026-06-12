#!/usr/bin/env python3
"""Boot EuroOS met een NVMe-controller (B2-harness). Verifieert init + identify +
read/write-zelftest + SMART via serial-nvme.log."""
import json, os, socket, subprocess, sys, time
IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
OUT = sys.argv[2] if len(sys.argv) > 2 else "nvme.png"
WAIT = int(sys.argv[3]) if len(sys.argv) > 3 else 150
NVME = os.environ.get("NVME", "/tmp/nvme.img")
OVMF = "/usr/share/ovmf/OVMF.fd"
QMP = "/tmp/ek-qmp-nvme.sock"
try: os.unlink(QMP)
except FileNotFoundError: pass
qemu = subprocess.Popen([
    "qemu-system-x86_64", "-machine", "q35", "-m", "256M", "-cpu", "qemu64,+smep,+smap",
    "-bios", OVMF, "-drive", f"format=raw,file={IMG}",
    "-drive", f"format=raw,file={NVME},if=none,id=nv",
    "-device", "nvme,drive=nv,serial=euronvme01",
    "-display", "none", "-serial", "file:serial-nvme.log",
    "-qmp", f"unix:{QMP},server,nowait",
    "-netdev", "user,id=n0,ipv4=on,ipv6=on", "-device", "virtio-net-pci,netdev=n0,disable-modern=on",
    "-no-reboot",
], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
for _ in range(50):
    if os.path.exists(QMP): break
    time.sleep(0.2)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(QMP)
fp = s.makefile("rwb", buffering=0)
def cmd(o):
    fp.write((json.dumps(o)+"\n").encode()); return json.loads(fp.readline().decode())
fp.readline(); cmd({"execute":"qmp_capabilities"})
print(f"[nvme] boot, wacht {WAIT}s...", flush=True); time.sleep(WAIT)
cmd({"execute":"screendump","arguments":{"filename":os.path.abspath(OUT),"format":"png"}})
cmd({"execute":"quit"}); time.sleep(1)
qemu.terminate()
try: qemu.wait(timeout=5)
except Exception: qemu.kill()
print("[nvme] klaar.", flush=True)
