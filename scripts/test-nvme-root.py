#!/usr/bin/env python3
"""Metal M2-3: install EuroOS to an NVMe disk, then boot standalone FROM it.

Phase A: boot the release image (UEFI from eurokernel.img) with a blank 512 MiB
         NVMe disk attached. The kernel detects blank NVMe + install media and
         installs a bootable, provisioned EuroOS onto it. Assert the install
         marker + that the live root became the NVMe EuroFS.
Phase B: boot with ONLY that NVMe disk (no eurokernel.img). UEFI boots the NVMe
         ESP; the kernel mounts its root on the NVMe EuroFS partition. Assert the
         standalone-NVMe-root marker + the interactive loop.

Proves the whole chain: install-to-NVMe + boot-from-NVMe + root-on-NVMe — a
modern NVMe-only laptop boots standalone. Never fakes success.
"""
import os
import subprocess
import sys
import tempfile
import time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
OVMF = "/usr/share/ovmf/OVMF.fd"
WORK = tempfile.mkdtemp(prefix="ek-nvmeroot-")
NVME = os.path.join(WORK, "nvmeroot.img")
BOOT_DEADLINE = 480


def serial(log):
    try:
        return open(log, errors="replace").read()
    except OSError:
        return ""


def run(args, log, want, deadline=BOOT_DEADLINE):
    """Boot QEMU with args; wait until `want` markers appear or the loop is up."""
    try:
        os.remove(log)
    except FileNotFoundError:
        pass
    q = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    t0 = time.time()
    up = False
    while time.time() - t0 < deadline:
        time.sleep(5)
        txt = serial(log)
        if "interactive loop started" in txt:
            up = True
            break
        if q.poll() is not None:
            break
    txt = serial(log)
    try:
        q.kill()
        q.wait(timeout=6)
    except Exception:
        pass
    return up, txt


def main():
    nvme_dev = ["-drive", f"format=raw,file={NVME},if=none,id=nv0",
                "-device", "nvme,drive=nv0,serial=euronvmeroot"]
    common = ["qemu-system-x86_64", "-machine", "q35", "-m", "512M",
              "-cpu", "qemu64,+smep,+smap", "-bios", OVMF,
              "-device", "qemu-xhci,id=xhci", "-device", "usb-kbd,bus=xhci.0",
              "-display", "none", "-no-reboot"]

    # A blank 512 MiB NVMe disk (>= the 128 MiB install minimum).
    subprocess.run(["truncate", "-s", "512M", NVME], check=True)

    # ── Phase A: install to NVMe ──
    logA = os.path.join(WORK, "install.log")
    argsA = common + ["-drive", f"format=raw,file={IMG}"] + nvme_dev + ["-serial", f"file:{logA}"]
    print("[nvmeroot] phase A: installing EuroOS to the blank NVMe disk", flush=True)
    upA, txtA = run(argsA, logA, None)
    install_ok = "EuroInstall → NVMe disk" in txtA and "OK (bootable" in txtA
    root_ok = "root = the freshly-installed EuroFS on NVMe" in txtA
    print(f"[nvmeroot] phase A: reached loop={upA}, install-marker={install_ok}, nvme-root={root_ok}")
    if not (upA and install_ok and root_ok):
        print("[nvmeroot] phase A FAILED; serial tail:")
        for l in txtA.splitlines()[-12:]:
            print("   | " + l)
        return 1

    # ── Phase B: boot standalone FROM the NVMe disk (no eurokernel.img) ──
    logB = os.path.join(WORK, "standalone.log")
    argsB = common + nvme_dev + ["-serial", f"file:{logB}"]
    print("[nvmeroot] phase B: booting standalone from the NVMe disk", flush=True)
    upB, txtB = run(argsB, logB, None)
    standalone_root = "root = the on-disk EuroFS on NVMe (standalone NVMe boot)" in txtB
    print(f"[nvmeroot] phase B: reached loop={upB}, standalone-nvme-root={standalone_root}")
    if not (upB and standalone_root):
        print("[nvmeroot] phase B FAILED; serial tail:")
        for l in txtB.splitlines()[-12:]:
            print("   | " + l)
        return 1

    print("[nvmeroot] PASS ✓ — installed to NVMe and booted standalone with root on NVMe")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
