#!/usr/bin/env python3
"""M5-2: prove the ACPI power button triggers a clean OS shutdown.

Boot, wait for the interactive loop, send QMP `system_powerdown` (which presses
the ACPI power button), and verify the guest performs an OS-controlled ACPI S5
shutdown: the '[acpi] power button pressed' marker AND the QEMU process exits
(SHUTDOWN event / process gone) rather than being killed. Never fakes success.
"""
import json
import os
import socket
import subprocess
import sys
import time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
OVMF = "/usr/share/ovmf/OVMF.fd"
SC = os.path.dirname(os.path.abspath(IMG))
LOG = os.path.join(SC, "powerbtn-serial.log")
QMP = "/tmp/ek-powerbtn.sock"


def main():
    for p in (LOG, QMP):
        try:
            os.remove(p)
        except FileNotFoundError:
            pass
    qemu = subprocess.Popen(
        ["qemu-system-x86_64", "-machine", "q35", "-m", "512M",
         "-cpu", "qemu64,+smep,+smap", "-bios", OVMF,
         "-drive", f"format=raw,file={IMG}",
         "-device", "qemu-xhci,id=xhci", "-device", "usb-kbd,bus=xhci.0",
         "-display", "none", "-serial", f"file:{LOG}",
         "-qmp", f"unix:{QMP},server,nowait", "-no-reboot"],
        stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

    def serial():
        try:
            return open(LOG, errors="replace").read()
        except OSError:
            return ""

    t0 = time.time()
    up = False
    while time.time() - t0 < 420:
        time.sleep(5)
        if "interactive loop started" in serial():
            up = True
            break
        if qemu.poll() is not None:
            break
    if not up:
        print("FAIL: never reached the interactive loop")
        print("\n".join(serial().splitlines()[-6:]))
        qemu.kill()
        return 1

    # Confirm the power button armed during boot.
    if "power button armed" not in serial():
        print("FAIL: power button was not armed at boot ('[acpi] power button armed' missing)")
        qemu.kill()
        return 1
    print("[powerbtn] loop up + power button armed; pressing it via QMP system_powerdown")

    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(QMP)
    s.settimeout(5)
    s.recv(65536)
    s.sendall(b'{"execute":"qmp_capabilities"}\n')
    time.sleep(0.2)
    s.recv(65536)
    s.sendall(b'{"execute":"system_powerdown"}\n')
    time.sleep(0.3)

    # Wait for the guest to shut itself down (process exits on ACPI S5 + -no-reboot).
    exited = False
    for _ in range(60):
        if qemu.poll() is not None:
            exited = True
            break
        time.sleep(1)
    txt = serial()
    pressed = "power button pressed" in txt
    clean = "shutting down system (ACPI S5" in txt

    ok = pressed and clean and exited
    print(f"[powerbtn] marker '[acpi] power button pressed': {pressed}")
    print(f"[powerbtn] clean ACPI S5 shutdown path: {clean}")
    print(f"[powerbtn] QEMU exited on its own (not killed): {exited}")
    if not exited:
        qemu.kill()
    print("[powerbtn]", "PASS ✓" if ok else "FAIL ✗")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
