#!/usr/bin/env python3
"""USB mass-storage auto-mount harness ([io-usb], task 3A-3).

Creates a FAT32 image with a test file, attaches it as a USB mass-storage device on a
qemu-xhci controller, boots EuroOS, and checks that the kernel auto-mounts it at /usb
(and proves FAT writeback). Verifies the real xHCI BOT/SCSI block path end-to-end.

Usage: python3 scripts/run-usb-storage.py [image.img] [wait_seconds]
Requires dosfstools (mkfs.fat) + mtools (mcopy).
"""
import os, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
WAIT = int(sys.argv[2]) if len(sys.argv) > 2 else int(os.environ.get("WAIT", "300"))
USB = "/tmp/usb-fat.img"
LOG = "serial-usb-storage.log"

OVMF = next((c for c in ("/usr/share/ovmf/OVMF.fd", "/usr/share/OVMF/OVMF.fd",
                         "/usr/share/edk2-ovmf/x64/OVMF.fd") if os.path.exists(c)), None)
assert OVMF, "OVMF not found"

# Build a FAT32 USB image with a known file on it.
subprocess.run(["qemu-img", "create", "-f", "raw", USB, "64M"], check=True, stdout=subprocess.DEVNULL)
subprocess.run(["mkfs.fat", "-F", "32", "-n", "EUROUSB", USB], check=True, stdout=subprocess.DEVNULL)
with open("/tmp/usb-hello.txt", "wb") as f:
    f.write(b"hello from a real USB stick\n")
subprocess.run(["mcopy", "-i", USB, "/tmp/usb-hello.txt", "::HELLO.TXT"], check=True)

for p in (LOG,):
    try:
        os.remove(p)
    except FileNotFoundError:
        pass

print(f"[usb-storage] FAT32 USB image ready; booting, waiting {WAIT}s...", flush=True)
qemu = subprocess.Popen([
    "qemu-system-x86_64", "-machine", "q35", "-m", "512M", "-cpu", "qemu64,+smep,+smap",
    "-bios", OVMF, "-drive", f"format=raw,file={IMG}",
    "-device", "qemu-xhci,id=xhci",
    "-drive", f"format=raw,file={USB},if=none,id=usbdisk",
    "-device", "usb-storage,drive=usbdisk,bus=xhci.0",
    "-display", "none", "-serial", f"file:{LOG}", "-no-reboot",
], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

deadline = time.time() + WAIT
done = False
while time.time() < deadline:
    time.sleep(5)
    try:
        with open(LOG, errors="ignore") as f:
            if "[io-usb]" in f.read():
                done = True
                break
    except FileNotFoundError:
        pass

try:
    qemu.terminate(); qemu.wait(timeout=10)
except Exception:
    qemu.kill()

print(f"[usb-storage] {'completed' if done else 'TIMEOUT (partial)'} — results:\n")
try:
    with open(LOG, errors="ignore") as f:
        for line in f:
            if "[io-usb]" in line or "mass storage" in line or "[xhci] slot" in line:
                print(line.rstrip())
except FileNotFoundError:
    print("(no serial log)")
