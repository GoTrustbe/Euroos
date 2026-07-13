#!/usr/bin/env python3
"""M1-4 (docs/SPRINT-PLAN-METAL.md): the q35 "metal matrix".

Boots the SAME release image against the QEMU device sets that mirror modern
real hardware, and asserts per leg that the kernel (a) reaches the interactive
loop and (b) prints the leg's driver/self-test markers. This is the no-hardware
regression net for the Metal phase: a driver or ECAM regression that would
break a real machine breaks a leg here first.

Legs:
  base    virtio disk/net (today's default)     -> [ecam] verified + [pci]
  nvme    + NVMe data disk                      -> [nvme] self-test OK
  ahci    + ICH9 AHCI with a SATA disk          -> [ahci] blank-disk write/read self-test (M2-2 driver)
  e1000e  + Intel e1000e NIC                    -> boot resilience (no driver yet)
  hda     + intel-hda with output codec         -> [hda]/[snd] init lines
  usb     + xhci: kbd, tablet, hub, usb-storage -> xHCI HID + mass-storage markers
  hwprobe base leg + typed `hwprobe` command    -> inventory lines over serial

Usage: python3 scripts/run-metal-matrix.py [image] [--legs a,b,c]
Exit code 0 = all legs pass. Never fakes success: a missing marker fails.
"""
import json
import os
import socket
import subprocess
import sys
import tempfile
import time

IMG = sys.argv[1] if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else "eurokernel.img"
OVMF = "/usr/share/ovmf/OVMF.fd"
WORK = tempfile.mkdtemp(prefix="ek-matrix-")
BOOT_DEADLINE = 420
RETRIES = 2  # the boot race is fixed (BUG-010); retries only guard infra flakes

AZ = {"a": "q", "q": "a", "z": "w", "w": "z", "m": "semicolon", " ": "spc"}


def leg_devices(leg):
    """Extra QEMU args per leg (on top of the common q35 + xhci-kbd base)."""
    if leg in ("base", "hwprobe"):
        return []
    if leg == "nvme":
        img = os.path.join(WORK, "nvme.img")
        subprocess.run(["truncate", "-s", "64M", img], check=True)
        return ["-drive", f"format=raw,file={img},if=none,id=nv0",
                "-device", "nvme,drive=nv0,serial=euromatrix1"]
    if leg == "ahci":
        # NB: ids like `sd0` are auto-reserved by QEMU (SD-card naming) and made
        # the whole VM abort at startup — hence the explicit `sata0`/`ahci2`.
        img = os.path.join(WORK, "sata.img")
        subprocess.run(["truncate", "-s", "64M", img], check=True)
        return ["-device", "ich9-ahci,id=ahci2",
                "-drive", f"format=raw,file={img},if=none,id=sata0",
                "-device", "ide-hd,drive=sata0,bus=ahci2.0"]
    if leg == "e1000e":
        return ["-netdev", "user,id=en0", "-device", "e1000e,netdev=en0"]
    if leg == "hda":
        return ["-device", "intel-hda,id=hda0", "-device", "hda-output,bus=hda0.0"]
    if leg == "usb":
        img = os.path.join(WORK, "usbdisk.img")
        subprocess.run(["truncate", "-s", "16M", img], check=True)
        return ["-device", "usb-hub,bus=xhci.0,port=3",
                "-drive", f"format=raw,file={img},if=none,id=ud0",
                "-device", "usb-storage,drive=ud0,bus=xhci.0,port=4"]
    raise SystemExit(f"unknown leg {leg}")


# (leg, [required serial markers], [forbidden serial markers])
LEGS = {
    "base": (["[ecam] PCIe config via ECAM", "[pci]",
              "[ahci] disk 0 read-only self-test (boot sector via DMA): OK",
              "interactive loop started"], ["FAILED ✗"]),
    "nvme": (["[nvme] self-test read/write", "[nvme] self-test 64 KiB PRP-list @ LBA 2000: OK",
              "[nvme] MSI-X delivery confirmed", "interactive loop started"],
             ["self-test: write FAILED", "self-test: read FAILED", "MISMATCH"]),
    "ahci": (["[ahci] disk 1", "self-test write/read: sector OK ✓ · 64 KiB OK ✓",
              "interactive loop started"], ["MISMATCH", "FAILED ✗"]),
    "e1000e": (["interactive loop started"], []),
    "hda": (["interactive loop started"], []),  # + dynamic check below: hda init line
    "usb": (["mass storage LIVE", "interactive loop started"], []),
    "hwprobe": (["interactive loop started"], []),
}


def boot(leg, extra, log, qmp):
    for p in (log, qmp):
        try:
            os.remove(p)
        except FileNotFoundError:
            pass
    args = ["qemu-system-x86_64", "-machine", "q35", "-m", "512M",
            "-cpu", "qemu64,+smep,+smap", "-bios", OVMF,
            "-drive", f"format=raw,file={IMG}",
            "-device", "qemu-xhci,id=xhci", "-device", "usb-kbd,bus=xhci.0",
            "-device", "usb-tablet,bus=xhci.0",
            "-display", "none", "-serial", f"file:{log}",
            "-qmp", f"unix:{qmp},server,nowait", "-no-reboot"] + extra
    return subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)


def serial(log):
    try:
        return open(log, errors="replace").read()
    except OSError:
        return ""


def qmp_type(qmp, text):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(qmp)
    s.settimeout(5)
    s.recv(65536)
    s.sendall(b'{"execute":"qmp_capabilities"}\n')
    time.sleep(0.2)
    s.recv(65536)
    for ch in text:
        qc = AZ.get(ch, ch)
        s.sendall((json.dumps({"execute": "send-key",
                               "arguments": {"keys": [{"type": "qcode", "data": qc}]}}) + "\n").encode())
        time.sleep(0.22)
        try:
            s.recv(65536)
        except OSError:
            pass
    s.sendall(b'{"execute":"send-key","arguments":{"keys":[{"type":"qcode","data":"ret"}]}}\n')
    time.sleep(0.2)
    s.close()


def run_leg(leg):
    need, forbid = LEGS[leg]
    extra = leg_devices(leg)
    log = os.path.join(WORK, f"{leg}.serial.log")
    qmp = os.path.join(WORK, f"{leg}.qmp.sock")
    for attempt in range(1, RETRIES + 1):
        qemu = boot(leg, extra, log, qmp)
        t0 = time.time()
        up = False
        while time.time() - t0 < BOOT_DEADLINE:
            time.sleep(5)
            if "interactive loop started" in serial(log):
                up = True
                break
        if up:
            break
        print(f"  [{leg}] boot attempt {attempt} did not reach the loop", flush=True)
        try:
            qemu.kill()
            qemu.wait(timeout=6)
        except Exception:
            pass
    if not up:
        print(f"  [{leg}] FAIL: never reached the interactive loop; serial tail:")
        for l in serial(log).splitlines()[-6:]:
            print("    | " + l)
        return False

    if leg == "hwprobe":
        time.sleep(10)
        qmp_type(qmp, "hwprobe")
        time.sleep(8)

    txt = serial(log)
    ok = True
    for m in need:
        if m not in txt:
            print(f"  [{leg}] FAIL: marker missing: {m!r}")
            ok = False
    for m in forbid:
        if m in txt:
            print(f"  [{leg}] FAIL: forbidden marker present: {m!r}")
            ok = False
    if leg == "hda" and "[hda]" not in txt and "[snd]" not in txt:
        print(f"  [{leg}] FAIL: no [hda]/[snd] init line with intel-hda attached")
        ok = False
    if leg == "hwprobe":
        if "EuroOS hwprobe" not in txt or "summary:" not in txt:
            print(f"  [{leg}] FAIL: hwprobe output not seen over serial")
            ok = False
        else:
            for l in txt.splitlines():
                if l.strip().startswith(("EuroOS hwprobe", "config-access", "pci ", "summary:", "acpi:")):
                    print("    | " + l.strip())
    try:
        qemu.kill()
        qemu.wait(timeout=6)
    except Exception:
        pass
    print(f"  [{leg}] {'PASS ✓' if ok else 'FAIL ✗'}", flush=True)
    return ok


def main():
    legs = list(LEGS)
    for i, a in enumerate(sys.argv):
        if a == "--legs":
            legs = sys.argv[i + 1].split(",")
    print(f"[matrix] image={IMG} legs={','.join(legs)} work={WORK}")
    results = {}
    for leg in legs:
        print(f"[matrix] leg: {leg}", flush=True)
        results[leg] = run_leg(leg)
    print("\n[matrix] result:")
    for leg, ok in results.items():
        print(f"  {leg:8} {'PASS' if ok else 'FAIL'}")
    if not all(results.values()):
        raise SystemExit(1)
    print("[matrix] ALL LEGS PASS ✓")


if __name__ == "__main__":
    main()
