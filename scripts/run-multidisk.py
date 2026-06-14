#!/usr/bin/env python3
"""Boot EuroOS with TWO virtio-blk disks (B3 multi-disk harness). Disk 0 =
root EuroFS (on disk1.img), disk 1 = extra mount (disk2.img). Headless, serial
to serial-multidisk.log, screenshot via QMP. Proves multiple disks +
mountpoints + df.
"""
import json, os, socket, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
OUT = sys.argv[2] if len(sys.argv) > 2 else "multidisk.png"
WAIT = int(sys.argv[3]) if len(sys.argv) > 3 else 150
D1 = os.environ.get("DISK1", "/tmp/disk1.img")
D2 = os.environ.get("DISK2", "/tmp/disk2.img")
OVMF = "/usr/share/ovmf/OVMF.fd"
QMP = "/tmp/ek-qmp-multidisk.sock"
try:
    os.unlink(QMP)
except FileNotFoundError:
    pass

qemu = subprocess.Popen([
    "qemu-system-x86_64", "-machine", "q35", "-m", "256M",
    "-smp", os.environ.get("SMP", "1"),
    "-cpu", "qemu64,+smep,+smap",
    "-bios", OVMF,
    "-drive", f"format=raw,file={IMG}",  # UEFI boot image (FAT32)
    # Two virtio-blk disks: root + extra mount.
    "-drive", f"format=raw,file={D1},if=none,id=d1",
    "-device", "virtio-blk-pci,drive=d1,disable-modern=on",
    "-drive", f"format=raw,file={D2},if=none,id=d2",
    "-device", "virtio-blk-pci,drive=d2,disable-modern=on",
    # Plus an NVMe disk (G2/B2: EuroFS-on-NVMe @ /nvme).
    "-drive", f"format=raw,file={os.environ.get('NVME', '/tmp/nvme.img')},if=none,id=nv",
    "-device", "nvme,drive=nv,serial=euronvme01",
    "-display", "none", "-serial", "file:serial-multidisk.log",
    "-qmp", f"unix:{QMP},server,nowait",
    "-netdev", "user,id=n0,ipv4=on,ipv6=on", "-device", "virtio-net-pci,netdev=n0,disable-modern=on",
    "-no-reboot",
], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

for _ in range(50):
    if os.path.exists(QMP):
        break
    time.sleep(0.2)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(QMP)
fp = s.makefile("rwb", buffering=0)

def cmd(obj):
    fp.write((json.dumps(obj) + "\n").encode())
    return json.loads(fp.readline().decode())

fp.readline()
cmd({"execute": "qmp_capabilities"})
print(f"[multidisk] boot, waiting {WAIT}s...", flush=True)
time.sleep(WAIT)
cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(OUT), "format": "png"}})
cmd({"execute": "quit"})
time.sleep(1)
qemu.terminate()
try:
    qemu.wait(timeout=5)
except Exception:
    qemu.kill()
print("[multidisk] done.", flush=True)
