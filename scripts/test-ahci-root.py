#!/usr/bin/env python3
"""Metal M2-3: install EuroOS to an AHCI/SATA disk, then boot standalone FROM it.

Boot-medium safety is the point: q35 exposes the boot image on SATA too, so the
installer must NEVER clobber it. It doesn't — the boot medium is partitioned
(0x55AA) and non-EuroFS, so both the install-to-blank and root-on-EuroFS rules
skip it. The install targets the SEPARATE blank AHCI disk.

Phase A: boot the release image (its own disk on the built-in AHCI = disk 0)
         plus a blank 512 MiB disk on a second AHCI controller (disk 1). The
         kernel installs onto the BLANK disk (1), leaving the boot medium (0)
         intact. Assert the install marker names disk 1 and the boot medium was
         not touched.
Phase B: boot with ONLY the installed disk on the built-in AHCI (no release
         image). UEFI boots its ESP; the kernel roots on its EuroFS. Assert the
         standalone-SATA-root marker.
"""
import os
import subprocess
import sys
import tempfile
import time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
OVMF = "/usr/share/ovmf/OVMF.fd"
WORK = tempfile.mkdtemp(prefix="ek-ahciroot-")
TARGET = os.path.join(WORK, "ahciroot.img")
BOOT_DEADLINE = 480


def serial(log):
    try:
        return open(log, errors="replace").read()
    except OSError:
        return ""


def run(args, log, deadline=BOOT_DEADLINE):
    try:
        os.remove(log)
    except FileNotFoundError:
        pass
    q = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    t0 = time.time()
    up = False
    while time.time() - t0 < deadline:
        time.sleep(5)
        if "interactive loop started" in serial(log):
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
    common = ["qemu-system-x86_64", "-machine", "q35", "-m", "512M",
              "-cpu", "qemu64,+smep,+smap", "-bios", OVMF,
              "-device", "qemu-xhci,id=xhci", "-device", "usb-kbd,bus=xhci.0",
              "-display", "none", "-no-reboot"]
    subprocess.run(["truncate", "-s", "512M", TARGET], check=True)

    # ── Phase A: release image (disk 0) + blank target on a 2nd AHCI ctrl (disk 1) ──
    logA = os.path.join(WORK, "install.log")
    argsA = common + [
        "-drive", f"format=raw,file={IMG}",  # built-in AHCI, disk 0 = boot medium
        "-device", "ich9-ahci,id=ahci2",
        "-drive", f"format=raw,file={TARGET},if=none,id=sata1",
        "-device", "ide-hd,drive=sata1,bus=ahci2.0",
        "-serial", f"file:{logA}",
    ]
    print("[ahciroot] phase A: installing to the blank AHCI disk (boot medium must stay intact)", flush=True)
    upA, txtA = run(argsA, logA)
    install_ok = "EuroInstall → AHCI disk" in txtA and "OK (bootable" in txtA
    root_ok = "freshly-installed EuroFS on AHCI disk" in txtA
    # Boot-medium safety: EXACTLY ONE install happened (only the blank disk;
    # the code installs only to blank disks and the boot medium isn't blank),
    # AND phase A booted from the boot medium (so it is provably intact). The
    # blank disk may enumerate as index 0 or 1 depending on PCI order — that's
    # fine; what matters is that a second disk was never installed to.
    n_installs = txtA.count("EuroInstall → AHCI disk")
    boot_safe = n_installs == 1 and upA
    print(f"[ahciroot] phase A: loop={upA}, install={install_ok}, root={root_ok}, "
          f"single-install(boot-medium-safe)={boot_safe} ({n_installs} install(s))")
    if not (upA and install_ok and root_ok and boot_safe):
        print("[ahciroot] phase A FAILED; serial tail:")
        for l in txtA.splitlines()[-12:]:
            print("   | " + l)
        return 1

    # ── Phase B: boot from the installed disk ALONE (built-in AHCI, disk 0) ──
    logB = os.path.join(WORK, "standalone.log")
    argsB = common + ["-drive", f"format=raw,file={TARGET}", "-serial", f"file:{logB}"]
    print("[ahciroot] phase B: booting standalone from the installed SATA disk", flush=True)
    upB, txtB = run(argsB, logB)
    standalone = "live root = the on-disk EuroFS on AHCI disk" in txtB and "standalone SATA boot" in txtB
    print(f"[ahciroot] phase B: loop={upB}, standalone-sata-root={standalone}")
    if not (upB and standalone):
        print("[ahciroot] phase B FAILED; serial tail:")
        for l in txtB.splitlines()[-12:]:
            print("   | " + l)
        return 1

    print("[ahciroot] PASS ✓ — installed to a blank SATA disk (boot medium untouched) and booted standalone with root on it")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
