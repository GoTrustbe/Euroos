#!/usr/bin/env python3
"""BUG-200 / BUG-201 (and the late self-test markers) on a SELF-TEST image, KVM, no network.

The self-test image installs itself onto a blank virtio disk 0.

MODE=plain (default): disk 0 is a normal blank disk, disk 1 a blank READ-ONLY disk.
  - the install on disk 0 must verify OK;
  - the wizard target list ([wizard-targets], BUG-201) must not offer disk 1 (read-only);
  - the late markers [randid] (BUG-086/087) and [nmi-bound] (BUG-202) must be OK.
MODE=inject: BUG-200 on its own. Disk 0 is NOT read-only, but QEMU's blkdebug fails every write to the blocks 16..143
  of the EuroFS root (sector 919552 + 8*block; 919552 = 2048 + 256 MiB ESP + two 96 MiB slots; the boot-file writer
  zeroes the first 16 blocks itself, with raw writes). The root blocks reach the cache without error, the flush fails, and "after remount"
  must not be satisfied by cache lines: that block is not on the medium, so the mount cannot succeed and
  hostname/user/euroid-store must read false.

usage: [MODE=plain|inject] ro-install-test.py IMAGE OUTDIR [CPUS]
"""
import os, subprocess, sys, time
IMG, OUT = sys.argv[1], os.path.abspath(sys.argv[2])
CPUS = sys.argv[3] if len(sys.argv) > 3 else "24-27"
MODE = os.environ.get("MODE", "plain")
os.makedirs(OUT, exist_ok=True)
tgt = os.path.join(OUT, "target0.img"); ro = os.path.join(OUT, "target1-ro.img"); log = os.path.join(OUT, f"serial-{MODE}.log")
for f in (tgt, ro):
    subprocess.run(["qemu-img", "create", "-f", "raw", f, "2G"], check=True, stdout=subprocess.DEVNULL)
drives = []
if MODE == "inject":
    conf = os.path.join(OUT, "blkdebug.conf")
    rules = []
    for blk in range(16, 144):   # the root blocks 16..143 (sectors 919552 + 8*blk): a flush of any of them fails
        rules += ["[inject-error]", 'event = "write_aio"', 'errno = "5"', f'sector = "{919552 + 8 * blk}"', 'immediately = "on"', ""]
    open(conf, "w").write(chr(10).join(rules))
    drives += ["-drive", f"id=t0,format=raw,file=blkdebug:{conf}:{tgt},if=none", "-device", "virtio-blk-pci,drive=t0,disable-modern=on"]
else:
    drives += ["-drive", f"id=t0,format=raw,file={tgt},if=none", "-device", "virtio-blk-pci,drive=t0,disable-modern=on",
               "-drive", f"id=t1,format=raw,file={ro},if=none,readonly=on", "-device", "virtio-blk-pci,drive=t1,disable-modern=on"]
p = subprocess.Popen(["taskset", "-c", CPUS, "qemu-system-x86_64", "-machine", "q35", "-enable-kvm", "-cpu", "host", "-m", "2048M", "-smp", "2",
    "-bios", "/usr/share/ovmf/OVMF.fd", "-display", "none", "-serial", f"file:{log}", "-no-reboot", "-nic", "none",
    "-drive", f"format=raw,file={IMG},snapshot=on"] + drives, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
def txt():
    try: return open(log, errors="replace").read()
    except FileNotFoundError: return ""
def wait(needle, secs):
    end = time.time() + secs
    while time.time() < end and needle not in txt() and p.poll() is None:
        time.sleep(2)
wait("[q1x3] EuroInstall", 900)
if MODE == "plain":
    wait("[nmi-bound]", 900)       # the late self-test markers
time.sleep(3)
t = txt()
if p.pid and p.pid > 1: p.kill()
res = []
def check(n, ok): res.append(ok); print(f"[ro-install] {'PASS' if ok else 'FAIL'}: {n}", flush=True)
def show(tag):
    l = [x for x in t.splitlines() if x.startswith(tag)]
    for x in l: print("[ro-install]   " + x[:330])
    return l
line = show("[q1x3] EuroInstall")
fa = show("[q1x3] target")
check("install reported", bool(line))
if MODE == "inject":
    after = line[0].split("after remount")[-1].split("→")[0] if line else "=true"
    check("after remount nothing reads back from the cache (hostname/user/euroid-store = false)", bool(line) and "=true" not in after)
    check("install verdict FAILED", bool(line) and line[0].rstrip().endswith("FAILED"))
else:
    check("install on the writable disk 0 verified OK", bool(line) and line[0].rstrip().split("→")[-1].strip().startswith("OK"))
    w = show("[wizard-targets]")
    check("wizard does not offer the read-only disk 1", bool(w) and all("FAILED" not in x for x in w))
    for tag in ("[randid] dhcp xid", "[randid] smb client challenge", "[nmi-bound]"):
        l = show(tag)
        check(f"marker {tag}", bool(l) and all("FAILED" not in x for x in l))
for f in (tgt, ro):
    os.unlink(f)
sys.exit(0 if all(res) else 1)
