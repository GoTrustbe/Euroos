#!/usr/bin/env python3
"""EuroUpdate over-the-air end-to-end test (three QEMU runs, TCG):
 1. install: boot the v1 live image with a blank 1 GiB virtio target disk -> EuroInstall writes GPT/ESP/slots/root.
 2. update:  boot the installed disk alone with user networking + 2 GB -> the periodic check finds v2 on
             https://euro-os.eu/update/, verifies, stages it to EuroSlot-B (serial: 'staged version').
 3. reboot:  boot the installed disk again -> the loader boots slot B from the partition (header + sha256),
             the kernel confirms the slot GOOD and reports the new version.
"""
import os, subprocess, sys, time, re
V1 = "/tmp/euroos-v1.img"; TARGET = "/tmp/euroos-ota-target.img"; OVMF = "/usr/share/ovmf/OVMF.fd"; OUT = "/tmp/euroos-ota"
os.makedirs(OUT, exist_ok=True)
def run(tag, args, ram, wait_for, timeout):
    serial = f"{OUT}/{tag}.serial"; open(serial, "w").close()
    q = subprocess.Popen(["qemu-system-x86_64", "-machine", "q35", "-cpu", "qemu64,+smep,+smap", "-m", ram, "-bios", OVMF,
                          "-display", "none", "-serial", f"file:{serial}", "-no-reboot"] + args, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    t0 = time.time(); hit = None
    while time.time() - t0 < timeout:
        time.sleep(5)
        txt = open(serial, errors="replace").read()
        for pat in wait_for:
            if re.search(pat, txt): hit = pat; break
        if hit or q.poll() is not None: break
    q.terminate()
    try: q.wait(timeout=10)
    except subprocess.TimeoutExpired: q.kill()
    txt = open(serial, errors="replace").read()
    print(f"[{tag}] {'HIT: '+hit if hit else 'TIMEOUT'} after {int(time.time()-t0)}s, {txt.count(chr(10))} serial lines", flush=True)
    return txt
step = sys.argv[1] if len(sys.argv) > 1 else "all"
BUS = sys.argv[2] if len(sys.argv) > 2 else "virtio"
def tgt_dev():
    if BUS == "ahci":
        return ["-drive", f"id=tgt,format=raw,file={TARGET},if=none", "-device", "ahci,id=ah", "-device", "ide-hd,drive=tgt,bus=ah.0"]
    if BUS == "nvme":
        return ["-drive", f"id=tgt,format=raw,file={TARGET},if=none", "-device", "nvme,drive=tgt,serial=euroos1"]
    return ["-drive", f"id=tgt,format=raw,file={TARGET},if=none", "-device", "virtio-blk-pci,drive=tgt,disable-modern=on"]
UPD_PAT = [r"\[euroupdate\] staged version", r"\[euroupdate\] up to date", r"\[euroupdate\] background check (failed|aborted|:)", r"\[euroupdate\] .*REFUSED",
           r"\[euroupdate\] could not reach", r"\[euroupdate\] version \d+ available", r"\[euroupdate\]   no connection", r"\[euroupdate\]   manifest HTTP"]
if step in ("install", "all"):
    subprocess.run(["qemu-img", "create", "-f", "raw", TARGET, "1G"], check=True, stdout=subprocess.DEVNULL)
    t = run("install", ["-drive", f"format=raw,file={V1}", "-drive", f"id=tgt,format=raw,file={TARGET},if=none",
                        "-device", "virtio-blk-pci,drive=tgt,disable-modern=on"], "1024M",
            [r"\[q1x3\] EuroInstall", r"too small"], 1200)
    for l in t.splitlines():
        if "[q1x3]" in l or "[q1x2]" in l or "EuroInstall" in l: print("   " + l[:200])
if step in ("update", "all"):
    t = run("update-"+BUS, tgt_dev() + ["-netdev", "user,id=n0", "-device", "virtio-net-pci,netdev=n0,disable-modern=on"], "2048M", UPD_PAT, 1800)
    for l in t.splitlines():
        if "[euroupdate]" in l or "[loader]" in l or "[tls]" in l: print("   " + l[:220])
if step in ("reboot", "all"):
    t = run("reboot-"+BUS, tgt_dev(), "2048M",
            [r"\[euroupdate\] slot . confirmed GOOD", r"\[loader\] FATAL", r"sha256 MISMATCH", r"LoadImage failed"], 600)
    for l in t.splitlines():
        if "[loader]" in l or "[euroupdate]" in l or "live root" in l or "RAM root" in l or "EUROOS_BUILD_VERSION" in l: print("   " + l[:220])
