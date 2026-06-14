#!/usr/bin/env python3
"""Big load / stress-test harness ([stress]).

Boots EuroOS (AHCI boot image) with SEVERAL virtio-blk disks of varying size,
arms the [stress] test (sentinel "EUROSTRESS" on disk 0, sector 0) and captures
the serial report. The test runs LATE in boot (after ring3/interrupts/VFS are up)
and exercises a sustained, multi-faceted workload:
  • external-disk write/rename/delete/rewrite churn (several rounds, integrity-checked)
  • cross-disk move (read disk0 → write disk1 → delete source)
  • fill the ROOT filesystem until full (the "boot disk is full" case) and recover
  • run multiple programs (synchronous runs + concurrent background tasks)
  • free-frame leak monitoring + a final root scrub

Disk roles (the boot path always claims the first two):
  disk0 → on-disk EuroFS ROOT (/)      — the "boot disk full" target
  disk1 → on-disk EuroFS /mnt
  disk2+ → FREE — formatted FAT32 and churned; cross-disk move needs two of them.
So provide >= 4 disks to exercise churn + cross-disk move.

Disks are sparse, so a large disk costs nothing until written. Usage:
  python3 scripts/run-stresstest.py [image.img] [wait_seconds]
Override sizes (MiB) with DISK_SIZES="32 32 64 128".
"""
import os, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
WAIT = int(sys.argv[2]) if len(sys.argv) > 2 else int(os.environ.get("WAIT", "600"))
OVMF = "/usr/share/ovmf/OVMF.fd"
SIZES_MIB = [int(x) for x in os.environ.get("DISK_SIZES", "32 16 16 16").split()]
SENTINEL = b"EUROSTRESS\n"

drives = []
for i, mib in enumerate(SIZES_MIB):
    path = f"/tmp/stress-{i}-{mib}m.img"
    with open(path, "wb") as f:
        f.truncate(mib * 1024 * 1024)
    if i == 0:
        with open(path, "r+b") as f:
            f.write(SENTINEL)
    drives += ["-drive", f"format=raw,file={path},if=none,id=d{i}",
               "-device", f"virtio-blk-pci,drive=d{i},disable-modern=on"]

print(f"[stress] disks (MiB): {SIZES_MIB}; booting, waiting {WAIT}s...", flush=True)
serial = "serial-stresstest.log"
qemu = subprocess.Popen(
    ["qemu-system-x86_64", "-machine", "q35", "-m", "512M", "-cpu", "qemu64,+smep,+smap",
     "-bios", OVMF, "-drive", f"format=raw,file={IMG}", "-display", "none",
     "-serial", f"file:{serial}"] + drives,
    stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

deadline = time.time() + WAIT
done = False
while time.time() < deadline:
    time.sleep(5)
    try:
        with open(serial, "r", errors="ignore") as f:
            if "[stress] ====== done" in f.read():
                done = True
                break
    except FileNotFoundError:
        pass

try:
    qemu.terminate(); qemu.wait(timeout=10)
except Exception:
    qemu.kill()

print(f"[stress] {'completed' if done else 'TIMEOUT (partial)'} — results:\n")
try:
    with open(serial, "r", errors="ignore") as f:
        for line in f:
            if "[stress]" in line:
                print(line.rstrip())
except FileNotFoundError:
    print("(no serial log)")
for i, mib in enumerate(SIZES_MIB):
    try:
        os.unlink(f"/tmp/stress-{i}-{mib}m.img")
    except FileNotFoundError:
        pass
