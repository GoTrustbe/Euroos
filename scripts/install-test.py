#!/usr/bin/env python3
"""AG-3c multidisk install test.

Run 1 (install): boot the LIVE EuroOS image (AHCI) with a BLANK virtio-blk
target disk attached; the kernel reads its own install media and writes a bootable
EuroOS to the target disk ([q1x2]).
Run 2 (standalone): boot the NOW-installed target disk alone — proves that it
boots EuroOS on its own. Takes a screenshot of it.
"""
import json, os, socket, subprocess, sys, time

IMG = "eurokernel.img"
TARGET = "/tmp/ag3-target.img"
QMP = "/tmp/ek-qmp-inst.sock"
OVMF = "/usr/share/ovmf/OVMF.fd"
W1 = int(os.environ.get("W1", "95"))   # install run
W2 = int(os.environ.get("W2", "115"))  # standalone boot run

# Fresh, blank 512 MiB target disk.
subprocess.run(["qemu-img", "create", "-f", "raw", TARGET, "512M"], check=True,
               stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

def boot(args, serial, wait, screenshot=None):
    for p in (QMP,):
        try: os.unlink(p)
        except FileNotFoundError: pass
    qemu = subprocess.Popen([
        "qemu-system-x86_64", "-machine", "q35", "-cpu", "qemu64,+smep,+smap",
        "-m", "256M", "-bios", OVMF, "-display", "none",
        "-serial", f"file:{serial}", "-qmp", f"unix:{QMP},server,nowait", "-no-reboot",
    ] + args, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    for _ in range(50):
        if os.path.exists(QMP): break
        time.sleep(0.2)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(QMP); f = s.makefile("rwb", buffering=0)
    def cmd(o): f.write((json.dumps(o)+"\n").encode()); return json.loads(f.readline().decode())
    f.readline(); cmd({"execute": "qmp_capabilities"})
    time.sleep(wait)
    if screenshot:
        cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(screenshot), "format": "png"}})
        time.sleep(2)
    try: cmd({"execute": "quit"})
    except Exception: pass
    qemu.wait(timeout=20)

print(f"[install-test] RUN 1: live EuroOS + blank virtio target disk, waiting {W1}s...", flush=True)
boot([
    "-drive", f"format=raw,file={IMG}",                                  # AHCI boot (live)
    "-drive", f"id=tgt,format=raw,file={TARGET},if=none",
    "-device", "virtio-blk-pci,drive=tgt,disable-modern=on",            # blank target disk
], "serial-install.log", W1)

print(f"[install-test] RUN 2: boot the installed target disk STANDALONE, waiting {W2}s...", flush=True)
boot([
    "-drive", f"format=raw,file={TARGET}",                               # AHCI boot from the install
], "serial-standalone.log", W2, screenshot="ag3-standalone.png")

print("[install-test] done — see serial-install.log ([q1x2]) + serial-standalone.log + ag3-standalone.png")
