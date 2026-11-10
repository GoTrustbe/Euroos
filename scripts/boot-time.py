#!/usr/bin/env python3
"""Timed boot of a EuroOS image: every serial line gets the host time since QEMU
started, the result line says when the kernel was entered and when the desktop
loop started. The kernel's own per-phase numbers are the `[boot-time]` lines in
the log (docs/BOOT-TIME.md).

usage: boot-time.py NAME IMG OUTDIR [options]
  --mode default|modern|legacy   default = as scripts/screenshot.py (SATA boot
                                 medium, legacy virtio-net); modern = virtio-blk,
                                 virtio-net, tablet and keyboard, disable-legacy=on
  --accel kvm|tcg   --smp N   --mem M   --pin CPUS   --nic virtio|e1000e|e1000
  --net restrict|open|none       user-net, restricted by default
  --hold S          keep running S seconds after the desktop
  --shot PNG        screendump before stopping
  --cmds FILE       shell commands for the serial console (scon), one per line;
                    their output is in the log as `[scon] ...`
  --type TEXT       type TEXT on the guest keyboard through QMP
  --extra "ARGS"    extra QEMU arguments

Writes OUTDIR/NAME.tlog. Everything it creates (boot image copy, QMP socket,
pid file) lives in OUTDIR/NAME.d, so several runs and several people do not
get in each other's way; it stops only the QEMU it started.
"""
import argparse, json, os, shutil, socket, subprocess, sys, threading, time
ap = argparse.ArgumentParser()
ap.add_argument("name"); ap.add_argument("img"); ap.add_argument("out")
ap.add_argument("--mode", default="default")
ap.add_argument("--accel", default="kvm")
ap.add_argument("--smp", default="2"); ap.add_argument("--mem", default="2G")
ap.add_argument("--hold", type=float, default=20); ap.add_argument("--timeout", type=float, default=600)
ap.add_argument("--shot"); ap.add_argument("--pin"); ap.add_argument("--net", default="restrict")
ap.add_argument("--marker", default="interactive loop started")
ap.add_argument("--type", help="text to type through QMP after the desktop (\n = Enter)")
ap.add_argument("--type-wait", type=float, default=15)
ap.add_argument("--cmds", help="file with shell commands for the serial console (scon), one per line")
ap.add_argument("--cmd-timeout", type=float, default=60)
ap.add_argument("--extra", default="", help="extra QEMU arguments (one string)")
ap.add_argument("--nic", default="virtio", help="virtio | e1000e | e1000 (default mode only)")
a = ap.parse_args()
w = os.path.join(a.out, a.name + ".d"); os.makedirs(w, exist_ok=True)
img = os.path.join(w, "boot.img"); shutil.copyfile(a.img, img)
qmp = os.path.join(w, "qmp.sock")
if os.path.exists(qmp): os.unlink(qmp)
netdev = "user,id=n0,restrict=on" if a.net == "restrict" else "user,id=n0"
if a.mode == "modern":
    p = ",disable-legacy=on"
    dev = ["-drive", f"if=none,id=root,format=raw,file={img}", "-device", f"virtio-blk-pci{p},drive=root,bootindex=0",
           "-netdev", netdev, "-device", f"virtio-net-pci{p},netdev=n0",
           "-device", "virtio-tablet-pci,disable-legacy=on", "-device", "virtio-keyboard-pci,disable-legacy=on"]
elif a.mode == "legacy":
    p = ",disable-modern=on"
    dev = ["-drive", f"if=none,id=root,format=raw,file={img}", "-device", f"virtio-blk-pci{p},drive=root,bootindex=0",
           "-netdev", netdev, "-device", f"virtio-net-pci{p},netdev=n0"]
else:  # as scripts/screenshot.py: SATA boot medium + legacy virtio-net
    nicdev = {"virtio": "virtio-net-pci,netdev=n0,disable-modern=on", "e1000e": "e1000e,netdev=n0", "e1000": "e1000,netdev=n0"}[a.nic]
    dev = ["-drive", f"format=raw,file={img}", "-netdev", netdev, "-device", nicdev]
if a.net == "none":
    i = dev.index("-netdev"); del dev[i:i+4]; dev += ["-nic", "none"]
acc = ["-enable-kvm", "-cpu", "host"] if a.accel == "kvm" else ["-cpu", "qemu64,+smep,+smap"]
cmd = ["qemu-system-x86_64", "-machine", "q35", *acc, "-m", a.mem, "-smp", a.smp, "-bios", "/usr/share/ovmf/OVMF.fd",
       *dev, "-vga", "std", "-display", "none", "-serial", "stdio", "-no-reboot", "-qmp", f"unix:{qmp},server,nowait"] + a.extra.split()
if a.pin: cmd = ["taskset", "-c", a.pin] + cmd
log = open(os.path.join(a.out, a.name + ".tlog"), "w")
t0 = time.monotonic()
q = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.PIPE if a.cmds else subprocess.DEVNULL)
ready = [0]
open(os.path.join(w, "qemu.pid"), "w").write(str(q.pid))
seen = {}
def reader():
    for raw in q.stdout:
        t = time.monotonic() - t0
        line = raw.decode("utf-8", "replace").rstrip("\r\n")
        log.write(f"{t:9.3f} {line}\n"); log.flush()
        if a.marker in line and "desk" not in seen: seen["desk"] = t
        if "[scon-ready]" in line: ready[0] += 1
        if "[euro] EuroKernel bring-up" in line and "k" not in seen: seen["k"] = t
th = threading.Thread(target=reader, daemon=True); th.start()
end = t0 + a.timeout
while "desk" not in seen and q.poll() is None and time.monotonic() < end: time.sleep(0.05)
def qcmd(f, o):
    f.write((json.dumps(o) + "\n").encode())
    while True:
        r = json.loads(f.readline().decode())
        if "return" in r or "error" in r: return r
res0 = f"{a.name}: kernel-entry {seen.get('k', -1):.2f}s desktop {seen.get('desk', -1):.2f}s"
res = res0
if "desk" in seen and a.cmds:
    def wait_ready(n, tmo):
        e = time.monotonic() + tmo
        while ready[0] < n and time.monotonic() < e and q.poll() is None: time.sleep(0.05)
        return ready[0] >= n
    wait_ready(1, 30)
    time.sleep(a.type_wait)
    n = ready[0]
    for c in open(a.cmds, encoding="utf-8").read().splitlines():
        c = c.strip()
        if not c or c.startswith("#"): continue
        q.stdin.write((c + chr(10)).encode()); q.stdin.flush()
        n += 1
        if not wait_ready(n, a.cmd_timeout):
            res += f" CMD-TIMEOUT({c})"; n = ready[0]
if "desk" in seen:
    if a.type or a.shot:
        s = socket.socket(socket.AF_UNIX); s.connect(qmp); f = s.makefile("rwb", buffering=0); f.readline()
        qcmd(f, {"execute": "qmp_capabilities"})
    if a.type:
        time.sleep(a.type_wait)
        km = {" ": "spc", "\n": "ret", ".": "dot", "-": "minus", "/": "slash", ":": None}
        for ch in a.type.replace("\n", "\n"):
            k = km.get(ch, ch)
            keys = [{"type": "qcode", "data": k}] if k else [{"type": "qcode", "data": "shift"}, {"type": "qcode", "data": "semicolon"}]
            qcmd(f, {"execute": "send-key", "arguments": {"keys": keys}}); time.sleep(0.15)
    time.sleep(a.hold)
    if a.shot:
        r = qcmd(f, {"execute": "screendump", "arguments": {"filename": os.path.abspath(a.shot), "format": "png"}})
        res += f" shot={'error' not in r}"
else:
    res += " NO-DESKTOP"
q.terminate()
try: q.wait(timeout=5)
except Exception: q.kill()
th.join(timeout=2); log.close(); os.unlink(img)
print(res, flush=True)
