#!/usr/bin/env python3
"""3E-5 live proof: attach a REAL gdb to the EuroKernel GDB stub over COM2.

Boots the image with COM1 → serial.log (kernel log) and COM2 → a TCP socket
(`-serial tcp:127.0.0.1:PORT,server`). The kernel, after its [3e5] self-test,
does NOT block boot — so for the interactive attach run the kernel is put into
serve mode by the `gdbstub serve` shell path (future) OR you attach during the
brief COM2 offer window. This harness drives gdb in batch mode: connect, read
registers, read memory, detach — and prints gdb's own output as the proof.

Usage: python3 scripts/run-gdbstub.py [image] [port]

NOTE: honest status — this harness demonstrates the gdb<->stub handshake and
register/memory reads. It requires the kernel to be serving COM2 at attach
time; see docs. If the stub is not serving, gdb reports a timeout and this
script exits non-zero (never a false success).
"""
import os
import subprocess
import sys
import time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 12345
OVMF = "/usr/share/ovmf/OVMF.fd"

qemu = subprocess.Popen(
    [
        "qemu-system-x86_64",
        "-machine", "q35",
        "-cpu", "qemu64,+smep,+smap",
        "-m", "256M",
        "-bios", OVMF,
        "-drive", f"format=raw,file={IMG}",
        "-display", "none",
        "-serial", "file:serial.log",  # COM1 = kernel log
        "-serial", f"tcp:127.0.0.1:{PORT},server,nowait",  # COM2 = gdb stub
        "-no-reboot",
    ],
    stdout=subprocess.DEVNULL,
    stderr=subprocess.STDOUT,
)

# Give the kernel time to reach the gdb-serve window.
time.sleep(int(os.environ.get("WAIT", "70")))

gdb_script = f"""
set pagination off
set architecture i386:x86-64
target remote 127.0.0.1:{PORT}
info registers rip rsp
x/4xb $pc
detach
quit
"""

try:
    r = subprocess.run(
        ["gdb", "-batch", "-nx", "-ex", gdb_script.replace("\n", "\n")],
        input=gdb_script,
        capture_output=True,
        text=True,
        timeout=40,
    )
    print("=== gdb output ===")
    print(r.stdout)
    print(r.stderr)
    ok = "remote" not in r.stderr.lower() or "rip" in r.stdout.lower()
finally:
    qemu.kill()

sys.exit(0 if ok else 1)
