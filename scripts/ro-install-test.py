#!/usr/bin/env python3
"""BUG-200 / BUG-201: boot a SELF-TEST image with a blank READ-ONLY virtio disk as disk 0.

The self-test image installs itself onto a blank virtio disk 0. The disk refuses every
write, so nothing may read back: the check "after remount" must say hostname/user/
euroid-store = false and the install FAILED (BUG-200: it read the write-back cache and
said true). The wizard target list must not offer the disk (BUG-201, [wizard-targets]).

usage: ro-install-test.py IMAGE OUTDIR [CPUS]   (KVM, restrict=on, no network)
"""
import os, re, subprocess, sys, time
IMG, OUT = sys.argv[1], os.path.abspath(sys.argv[2])
CPUS = sys.argv[3] if len(sys.argv) > 3 else "24-27"
os.makedirs(OUT, exist_ok=True)
tgt = os.path.join(OUT, "ro-target.img"); log = os.path.join(OUT, "serial-ro.log")
subprocess.run(["qemu-img", "create", "-f", "raw", tgt, "2G"], check=True, stdout=subprocess.DEVNULL)
p = subprocess.Popen(["taskset", "-c", CPUS, "qemu-system-x86_64", "-machine", "q35", "-enable-kvm", "-cpu", "host", "-m", "2048M", "-smp", "2",
    "-bios", "/usr/share/ovmf/OVMF.fd", "-display", "none", "-serial", f"file:{log}", "-no-reboot", "-nic", "none",
    "-drive", f"format=raw,file={IMG},snapshot=on", "-drive", f"id=tgt,format=raw,file={tgt},if=none,readonly=on",
    "-device", "virtio-blk-pci,drive=tgt,disable-modern=on"], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
def txt():
    try: return open(log, errors="replace").read()
    except FileNotFoundError: return ""
end = time.time() + 900
while time.time() < end and "[q1x3] EuroInstall" not in txt() and "[q1x3] target" not in txt() and p.poll() is None:
    time.sleep(2)
end = time.time() + 900          # the late self-test markers ([wizard-targets], [nmi-bound], [randid])
while time.time() < end and "[nmi-bound]" not in txt() and p.poll() is None:
    time.sleep(2)
time.sleep(3)
t = txt()
if p.pid and p.pid > 1: p.kill()
res = []
def check(n, ok): res.append(ok); print(f"[ro-install] {PASS if ok else FAIL}: {n}", flush=True)
line = next((l for l in t.splitlines() if l.startswith("[q1x3] EuroInstall")), "")
print("[ro-install] " + line[:300])
check("install reported on the read-only disk", bool(line))
check("after remount nothing reads back (hostname/user/euroid-store = false)",
      bool(line) and "=true" not in line.split("after remount")[-1].split("→")[0])
check("install verdict FAILED", bool(line) and line.rstrip().endswith("FAILED"))
w = [l for l in t.splitlines() if l.startswith("[wizard-targets]")]
print("\n".join("[ro-install] " + l for l in w))
check("wizard does not offer the read-only disk", bool(w) and all("FAILED" not in l for l in w))
for tag in ("[randid] dhcp xid", "[randid] smb client challenge", "[nmi-bound]"):
    l = [x for x in t.splitlines() if x.startswith(tag)]
    print(chr(10).join("[ro-install] " + x for x in l))
    check(f"marker {tag}", bool(l) and all("FAILED" not in x for x in l))
os.unlink(tgt)
sys.exit(0 if all(res) else 1)
