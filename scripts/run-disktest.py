#!/usr/bin/env python3
"""Multi-disk load + functional test harness.

Boots EuroOS (AHCI boot image) with SEVERAL virtio-blk disks of different sizes,
arms the destructive [mdisk] test (sentinel on disk 0), and captures the serial
report: per-disk format/fill/verify/delete/reformat timing + a cross-disk copy.

Disks are sparse, so a 2 GiB disk costs almost nothing until written. Usage:
  python3 scripts/run-disktest.py [image.img] [wait_seconds]
Override sizes (MiB) with DISK_SIZES="8 64 512 2048".
"""
import os, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
WAIT = int(sys.argv[2]) if len(sys.argv) > 2 else int(os.environ.get("WAIT", "260"))
OVMF = "/usr/share/ovmf/OVMF.fd"
SIZES_MIB = [int(x) for x in os.environ.get("DISK_SIZES", "8 64 512 2048").split()]
SENTINEL = b"EURODISKTEST\n"

drives = []
for i, mib in enumerate(SIZES_MIB):
    path = f"/tmp/disktest-{i}-{mib}m.img"
    # Fresh sparse disk.
    with open(path, "wb") as f:
        f.truncate(mib * 1024 * 1024)
    if i == 0:
        # Arm the test: sentinel on disk 0, sector 0.
        with open(path, "r+b") as f:
            f.write(SENTINEL)
    drives += ["-drive", f"format=raw,file={path},if=none,id=d{i}",
               "-device", f"virtio-blk-pci,drive=d{i},disable-modern=on"]

print(f"[disktest] disks (MiB): {SIZES_MIB}; booting, waiting {WAIT}s...", flush=True)
serial = "serial-disktest.log"
qemu = subprocess.Popen(
    ["qemu-system-x86_64", "-machine", "q35", "-m", "512M", "-cpu", "qemu64,+smep,+smap",
     "-bios", OVMF, "-drive", f"format=raw,file={IMG}", "-display", "none",
     "-serial", f"file:{serial}"] + drives,
    stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

# Poll the serial log for the done marker (or timeout).
deadline = time.time() + WAIT
done = False
while time.time() < deadline:
    time.sleep(5)
    try:
        with open(serial, "r", errors="ignore") as f:
            if "[mdisk] === done" in f.read():
                done = True
                break
    except FileNotFoundError:
        pass

try:
    qemu.terminate(); qemu.wait(timeout=10)
except Exception:
    qemu.kill()

print(f"[disktest] {'completed' if done else 'TIMEOUT (partial)'} — results:\n")
try:
    with open(serial, "r", errors="ignore") as f:
        for line in f:
            if "[mdisk]" in line:
                print(line.rstrip())
except FileNotFoundError:
    print("(no serial log)")
for i, mib in enumerate(SIZES_MIB):
    try:
        os.unlink(f"/tmp/disktest-{i}-{mib}m.img")
    except FileNotFoundError:
        pass
