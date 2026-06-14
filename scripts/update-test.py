#!/usr/bin/env python3
"""AH-2 A/B self-update test (3 runs, the same target disk).

RUN1 install : live EuroOS + BLANK virtio disk → install (slot_config → A).
RUN2 update  : live EuroOS + the INSTALLED disk → stage slot B ([upd2], slot_config → B).
RUN3 boot-B  : boot the disk standalone → the loader honors slot_config → boots slot B.
"""
import json, os, socket, subprocess, time

IMG = "eurokernel.img"
TGT = "/tmp/ah2-target.img"
QMP = "/tmp/ek-qmp-upd.sock"
OVMF = "/usr/share/ovmf/OVMF.fd"
W1, W2, W3 = int(os.environ.get("W1", "95")), int(os.environ.get("W2", "95")), int(os.environ.get("W3", "115"))

subprocess.run(["qemu-img", "create", "-f", "raw", TGT, "512M"], check=True,
               stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

def boot(args, serial, wait, screenshot=None):
    try: os.unlink(QMP)
    except FileNotFoundError: pass
    q = subprocess.Popen([
        "qemu-system-x86_64", "-machine", "q35", "-cpu", "qemu64,+smep,+smap",
        "-m", "256M", "-bios", OVMF, "-display", "none",
        "-serial", f"file:{serial}", "-qmp", f"unix:{QMP},server,nowait", "-no-reboot",
    ] + args, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    for _ in range(50):
        if os.path.exists(QMP): break
        time.sleep(0.2)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(QMP)
    f = s.makefile("rwb", buffering=0)
    def cmd(o): f.write((json.dumps(o)+"\n").encode()); return json.loads(f.readline().decode())
    f.readline(); cmd({"execute": "qmp_capabilities"})
    time.sleep(wait)
    if screenshot:
        cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(screenshot), "format": "png"}}); time.sleep(2)
    try: cmd({"execute": "quit"})
    except Exception: pass
    q.wait(timeout=20)

live_plus_target = [
    "-drive", f"format=raw,file={IMG}",
    "-drive", f"id=tgt,format=raw,file={TGT},if=none",
    "-device", "virtio-blk-pci,drive=tgt,disable-modern=on",
]
print(f"[update-test] RUN1 install (slot A), {W1}s...", flush=True)
boot(live_plus_target, "serial-upd-install.log", W1)
print(f"[update-test] RUN2 stage A/B update (slot B), {W2}s...", flush=True)
boot(live_plus_target, "serial-upd-stage.log", W2)
print(f"[update-test] RUN3 boot the updated disk STANDALONE, {W3}s...", flush=True)
boot(["-drive", f"format=raw,file={TGT}"], "serial-upd-bootb.log", W3, screenshot="ah2-slotb.png")
print("[update-test] done — RUN2 [upd2], RUN3 loader 'boot slot B' + ah2-slotb.png")
