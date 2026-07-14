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
  e1000e  + Intel e1000e NIC                    -> full DHCP/ping net suite on the e1000 driver (M3-1)
  hda     + intel-hda with output codec         -> [hda]/[snd] init lines
  usb     + xhci: kbd, tablet, hub, usb-storage -> xHCI HID + mass-storage markers
  usbhub  keyboard ONLY behind a usb-hub         -> typing works through the hub (M4-1)
  usbnet  ONLY NIC = usb-net (CDC-ECM)            -> DHCP/ping over USB ethernet (M3-3)
  power   base leg + ACPI power-button press       -> armed + clean S5 shutdown (M5-2)
  tpm     + swtpm tpm-tis @ 0xFED40000             -> real TPM2 seal/unseal to PCR16 via TIS (M6-1)
  tpmcrb  + swtpm tpm-crb @ 0xFED40000             -> same, via the CRB fTPM/PTT interface (M6-1)
  printer + host mock IPP server via guestfwd      -> IPP Print-Job round-trip (M7-1)
  scan    + host mock eSCL scanner on :8631        -> driverless scan round-trip → image (M7-2)
  nvmeroot install to a blank NVMe → boot from it  -> standalone boot with root on NVMe (M2-3)
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
import shutil

IMG = sys.argv[1] if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else "eurokernel.img"
OVMF = "/usr/share/ovmf/OVMF.fd"
WORK = tempfile.mkdtemp(prefix="ek-matrix-")
BOOT_DEADLINE = 420
RETRIES = 2  # the boot race is fixed (BUG-010); retries only guard infra flakes

AZ = {"a": "q", "q": "a", "z": "w", "w": "z", "m": "semicolon", " ": "spc"}

# Side processes (swtpm / mock IPP server) started per-leg, torn down after.
SIDE = {}


def leg_devices(leg):
    """Extra QEMU args per leg (on top of the common q35 + xhci-kbd base)."""
    if leg in ("base", "hwprobe", "power", "printer", "scan"):
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
    if leg == "usbhub":
        # The ONLY keyboard sits behind a hub: typing proves hub enumeration
        # end-to-end (route strings + interrupt-IN through the hub).
        return ["-device", "usb-hub,bus=xhci.0,port=3",
                "-device", "usb-kbd,bus=xhci.0,port=3.1"]
    if leg == "usbnet":
        # ONLY NIC = a CDC-ECM USB-ethernet function (phone-tethering class):
        # -nic none suppresses the default e1000e, so the net stack must run
        # over the USB pipes or not at all.
        # restrict=on: slirp still runs DHCP + answers gateway pings, but the
        # guest can't reach host services (the OTA server on :8722). That keeps
        # the usbnet leg a pure connectivity test (DHCP + ping, like the e1000e
        # leg) instead of gating on a heavy image-staging TCP session, which is
        # slow over emulated USB under 60x TCG (an emulator tax, not a driver
        # defect — the CDC-ECM link itself is proven by DHCP + ping).
        return ["-nic", "none",
                "-netdev", "user,id=un0,restrict=on", "-device", "usb-net,netdev=un0,bus=xhci.0"]
    if leg == "usb":
        img = os.path.join(WORK, "usbdisk.img")
        subprocess.run(["truncate", "-s", "16M", img], check=True)
        return ["-device", "usb-hub,bus=xhci.0,port=3",
                "-drive", f"format=raw,file={img},if=none,id=ud0",
                "-device", "usb-storage,drive=ud0,bus=xhci.0,port=4"]
    if leg in ("tpm", "tpmcrb"):
        # swtpm is started in run_leg (needs teardown); here just attach it.
        # tpm = TIS (discrete-chip interface); tpmcrb = CRB (fTPM/PTT interface).
        sock = SIDE.get("tpm_sock")
        dev = "tpm-crb" if leg == "tpmcrb" else "tpm-tis"
        return ["-chardev", f"socket,id=chrtpm,path={sock}",
                "-tpmdev", "emulator,id=tpm0,chardev=chrtpm",
                "-device", f"{dev},tpmdev=tpm0"]
    if leg == "printer":
        # No special netdev: slirp already forwards guest -> 10.0.2.2:631 to the
        # host's service on :631 (same mechanism the OTA server on :8722 uses).
        # The mock IPP server (start_side) listens on the host at :631.
        return []
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
    "e1000e": (["NIC: e1000 MAC", "[net] DHCP OFFER:",
                "PING 10.0.2.2: echo-reply OK ✓", "interactive loop started"], []),
    "hda": (["interactive loop started"], []),  # + dynamic check below: hda init line
    "usb": (["mass storage LIVE", "interactive loop started"], []),
    "usbhub": (["(behind hub)", "via hub", "interactive loop started"], []),
    "usbnet": (["USB ethernet (CDC-ECM) LIVE", "NIC: usb-ethernet (CDC-ECM)",
                "[net] DHCP OFFER:", "PING 10.0.2.2: echo-reply OK ✓",
                "interactive loop started"], []),
    "hwprobe": (["interactive loop started"], []),
    "scan": (["[m72] EuroScan eSCL ✓", "EuroScan Virtual 3000", "NextDocument", "interactive loop started"], []),
    "nvmeroot": ([], []),  # handled specially in run_leg (two-phase install→boot)
    "power": (["[acpi] power button armed", "[acpi-pwr]", "interactive loop started"], []),
    "tpm": (["TPM 2.0 TIS @ 0xfed40000", "[3e1] EnrollFde EXECUTED", "unseal-roundtrip=true",
             "interactive loop started"], ["unseal-roundtrip=false"]),
    "tpmcrb": (["TPM 2.0 CRB @ 0xfed40000", "[3e1] EnrollFde EXECUTED", "unseal-roundtrip=true",
                "interactive loop started"], ["unseal-roundtrip=false"]),
    "printer": (["[bb4] EuroPrint IPP-over-TCP", "Print-Job status=0x0000 (ok=true)",
                 "interactive loop started"], ["ok=false"]),
}


def boot(leg, extra, log, qmp):
    for p in (log, qmp):
        try:
            os.remove(p)
        except FileNotFoundError:
            pass
    base_kbd = [] if leg == "usbhub" else ["-device", "usb-kbd,bus=xhci.0"]
    args = (["qemu-system-x86_64", "-machine", "q35", "-m", "512M",
            "-cpu", "qemu64,+smep,+smap", "-bios", OVMF,
            "-drive", f"format=raw,file={IMG}",
            "-device", "qemu-xhci,id=xhci"] + base_kbd +
            ["-device", "usb-tablet,bus=xhci.0",
            "-display", "none", "-serial", f"file:{log}",
            "-qmp", f"unix:{qmp},server,nowait", "-no-reboot"] + extra)
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


def start_side(leg):
    if leg in ("tpm", "tpmcrb"):
        state = tempfile.mkdtemp(prefix="ek-swtpm-")
        sock = os.path.join(state, "sock")
        p = subprocess.Popen(["swtpm", "socket", "--tpm2", "--tpmstate", f"dir={state}",
                              "--ctrl", f"type=unixio,path={sock}", "--log", "level=1"],
                             stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
        for _ in range(50):
            if os.path.exists(sock):
                break
            time.sleep(0.1)
        SIDE["tpm_proc"] = p
        SIDE["tpm_state"] = state
        SIDE["tpm_sock"] = sock
    elif leg == "scan":
        script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mock-escl-server.py")
        p = subprocess.Popen([sys.executable, script, "--port", "8631"],
                             stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
        time.sleep(1.2)
        SIDE["escl_proc"] = p
    elif leg == "printer":
        spool = tempfile.mkdtemp(prefix="ek-ipp-")
        script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mock-ipp-server.py")
        p = subprocess.Popen(["sudo", "-n", sys.executable, script, "--port", "631", "--spool", spool],
                             stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
        time.sleep(1.5)
        SIDE["ipp_proc"] = p
        SIDE["ipp_spool"] = spool


def stop_side(leg):
    ipp = SIDE.pop("ipp_proc", None)
    if ipp:
        # The server runs under sudo; pkill it by script name.
        subprocess.run(["sudo", "-n", "pkill", "-f", "mock-ipp-server.py"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            ipp.wait(timeout=6)
        except Exception:
            pass
    for key in ("tpm_proc", "escl_proc"):
        p = SIDE.pop(key, None)
        if p:
            try:
                p.kill()
                p.wait(timeout=6)
            except Exception:
                pass
    st = SIDE.pop("tpm_state", None)
    if st:
        shutil.rmtree(st, ignore_errors=True)
    SIDE.pop("tpm_sock", None)
    SIDE.pop("ipp_port", None)
    SIDE.pop("ipp_spool", None)


def run_leg(leg):
    if leg == "nvmeroot":
        # Two-phase (install to NVMe → standalone boot from NVMe) — delegated to
        # the dedicated harness so the disk image persists across both boots.
        script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "test-nvme-root.py")
        r = subprocess.run([sys.executable, script, IMG], capture_output=True, text=True)
        for l in r.stdout.splitlines():
            print("  " + l, flush=True)
        ok = r.returncode == 0
        print(f"  [nvmeroot] {'PASS ✓' if ok else 'FAIL ✗'}", flush=True)
        return ok
    need, forbid = LEGS[leg]
    start_side(leg)
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
        stop_side(leg)
        return False

    if leg == "hwprobe":
        time.sleep(10)
        qmp_type(qmp, "hwprobe")
        time.sleep(8)
    if leg == "usbhub":
        time.sleep(10)
        qmp_type(qmp, "uname")
        time.sleep(8)
    if leg == "power":
        time.sleep(8)
        # Press the ACPI power button; the guest must shut itself down cleanly.
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(qmp); s.settimeout(5)
        s.recv(65536); s.sendall(b'{"execute":"qmp_capabilities"}\n'); time.sleep(0.2); s.recv(65536)
        s.sendall(b'{"execute":"system_powerdown"}\n'); time.sleep(0.3); s.close()
        for _ in range(60):
            if qemu.poll() is not None:
                break
            time.sleep(1)

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
    if leg == "usbhub" and "[e2e] $ uname" not in txt:
        print(f"  [{leg}] FAIL: typed uname never echoed — hub keyboard not delivering")
        ok = False
    if leg == "power":
        txt = serial(log)
        if "power button pressed" not in txt or "shutting down system (ACPI S5" not in txt:
            print(f"  [{leg}] FAIL: power button did not trigger a clean ACPI S5 shutdown")
            ok = False
        if qemu.poll() is None:
            print(f"  [{leg}] FAIL: guest did not power off on its own")
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
    stop_side(leg)
    print(f"  [{leg}] {'PASS ✓' if ok else 'FAIL ✗'}", flush=True)
    return ok


def main():
    # `printer` needs a privileged IPP endpoint on host :631 (slirp forwards
    # guest -> 10.0.2.2:631 there). It is opt-in via --legs printer; the default
    # sweep is the fully self-contained set.
    legs = [l for l in LEGS if l != "printer"]
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
