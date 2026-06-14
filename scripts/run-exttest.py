#!/usr/bin/env python3
"""IO-7 harness: boot EuroOS with a real ext4 disk on virtio-blk disk 0 and capture the
[io7] result (the kernel detects ext, mounts it read-only, reads a file).

The disk image is the committed mkfs.ext4 fixture. Usage:
  python3 scripts/run-exttest.py [image.img] [wait_seconds]
"""
import os, shutil, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
WAIT = int(sys.argv[2]) if len(sys.argv) > 2 else 120
OVMF = "/usr/share/ovmf/OVMF.fd"
FIXTURE = "crates/euroext/testdata/ext4.img"
DISK = "/tmp/exttest-disk0.img"
serial = "serial-exttest.log"

shutil.copyfile(FIXTURE, DISK)  # fresh copy so the (read-only) test never mutates the fixture
print(f"[exttest] booting with ext4 disk0 ({os.path.getsize(DISK)//1024} KiB), waiting {WAIT}s...", flush=True)

q = subprocess.Popen(
    ["qemu-system-x86_64", "-machine", "q35", "-m", "256M", "-cpu", "qemu64,+smep,+smap",
     "-bios", OVMF, "-drive", f"format=raw,file={IMG}", "-display", "none",
     "-serial", f"file:{serial}",
     "-drive", f"format=raw,file={DISK},if=none,id=d0",
     "-device", "virtio-blk-pci,drive=d0,disable-modern=on"],
    stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

deadline = time.time() + WAIT
while time.time() < deadline:
    time.sleep(5)
    try:
        if "[io7]" in open(serial, errors="ignore").read():
            break
    except FileNotFoundError:
        pass
try:
    q.terminate(); q.wait(timeout=10)
except Exception:
    q.kill()

print("[exttest] result:")
for line in open(serial, errors="ignore"):
    if "[io7]" in line:
        print(line.rstrip())
os.unlink(DISK)
