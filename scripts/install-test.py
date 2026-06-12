#!/usr/bin/env python3
"""AG-3c multidisk-installtest.

Run 1 (installeren): boot de LIVE EuroOS-image (AHCI) met een BLANCO virtio-blk-
doelschijf erbij; de kernel leest zijn eigen install-media en schrijft een bootbare
EuroOS naar de doelschijf ([q1x2]).
Run 2 (standalone): boot de NU-geïnstalleerde doelschijf alleen — bewijst dat ze
zelfstandig EuroOS opstart. Maakt er een screenshot van.
"""
import json, os, socket, subprocess, sys, time

IMG = "eurokernel.img"
TARGET = "/tmp/ag3-target.img"
QMP = "/tmp/ek-qmp-inst.sock"
OVMF = "/usr/share/ovmf/OVMF.fd"
W1 = int(os.environ.get("W1", "95"))   # installeer-run
W2 = int(os.environ.get("W2", "115"))  # standalone-boot-run

# Verse, blanco 512 MiB doelschijf.
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

print(f"[install-test] RUN 1: live EuroOS + blanco virtio-doelschijf, wacht {W1}s...", flush=True)
boot([
    "-drive", f"format=raw,file={IMG}",                                  # AHCI boot (live)
    "-drive", f"id=tgt,format=raw,file={TARGET},if=none",
    "-device", "virtio-blk-pci,drive=tgt,disable-modern=on",            # blanco doelschijf
], "serial-install.log", W1)

print(f"[install-test] RUN 2: boot de geïnstalleerde doelschijf STANDALONE, wacht {W2}s...", flush=True)
boot([
    "-drive", f"format=raw,file={TARGET}",                               # AHCI boot van de install
], "serial-standalone.log", W2, screenshot="ag3-standalone.png")

print("[install-test] klaar — zie serial-install.log ([q1x2]) + serial-standalone.log + ag3-standalone.png")
