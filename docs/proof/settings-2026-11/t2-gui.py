#!/usr/bin/env python3
"""T2 GUI harness: boot an EuroOS image under QEMU (TCG) with a virtio tablet and
keyboard, wait for the desktop, run an input script over QMP, take screenshots.

Own work directory and own QMP socket per run (never /tmp/ek-qmp.sock).

usage: t2-gui.py WORKDIR IMAGE SCRIPT [--disk FILE] [--boot-disk] [--mem 1G] [--smp 2]
                 [--wait-marker TEXT] [--timeout SEC] [--no-copy]
  IMAGE is copied into WORKDIR first (unless --no-copy), so a run never writes
  the image it was given. --boot-disk boots IMAGE as the only (AHCI) disk and
  does not copy it: for an installed system that must keep what it writes.

Script lines:
  move X Y | click | clickat X Y | key QCODE[-QCODE...] | type TEXT | wait SEC
  shot NAME.png | waitlog TEXT [SEC] | mark | expect TEXT (in the log since mark)
`type` sends US-layout qcodes; what the guest makes of them depends on its layout.
"""
import json, os, select, shutil, socket, subprocess, sys, time

args = sys.argv[1:]
def opt(name, default=None, flag=False):
    if name in args:
        i = args.index(name)
        if flag:
            args.pop(i); return True
        v = args[i + 1]; del args[i:i + 2]; return v
    return False if flag else default

DISK = opt("--disk")
BOOT_DISK = opt("--boot-disk", flag=True)
NO_COPY = opt("--no-copy", flag=True)
MEM = opt("--mem", "1G")
SMP = opt("--smp", "2")
MARKER = opt("--wait-marker", "[desktop] interactive loop started")
TIMEOUT = int(opt("--timeout", "1500"))
WORK, IMG, SCRIPT = args[0], args[1], args[2]
SW, SH = 1920, 1080
os.makedirs(WORK, exist_ok=True)
LOG = os.path.join(WORK, "serial.log")
QMP = os.path.join(WORK, "qmp.sock")
for p in (QMP, LOG):
    try: os.unlink(p)
    except FileNotFoundError: pass

if BOOT_DISK or NO_COPY:
    boot = IMG
else:
    boot = os.path.join(WORK, "boot.img")
    shutil.copyfile(IMG, boot)
cmdline = ["nice", "-n", "10", "qemu-system-x86_64", "-machine", "q35", "-cpu", "qemu64,+smep,+smap",
           "-m", MEM, "-smp", SMP, "-bios", "/usr/share/ovmf/OVMF.fd",
           "-drive", f"format=raw,file={boot}",
           "-device", "virtio-tablet-pci,disable-legacy=on", "-device", "virtio-keyboard-pci,disable-legacy=on",
           "-netdev", "user,id=n0", "-device", "virtio-net-pci,netdev=n0",
           "-vga", "std", "-display", "none", "-serial", f"file:{LOG}", "-no-reboot",
           "-qmp", f"unix:{QMP},server,nowait"]
if DISK:
    cmdline += ["-drive", f"id=tgt,format=raw,file={DISK},if=none", "-device", "virtio-blk-pci,drive=tgt,disable-modern=on"]
qemu = subprocess.Popen(cmdline, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
open(os.path.join(WORK, "qemu.pid"), "w").write(str(qemu.pid))
print(f"[t2-gui] qemu pid {qemu.pid}", flush=True)
for _ in range(100):
    if os.path.exists(QMP): break
    time.sleep(0.2)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(QMP)
f = s.makefile("rwb", buffering=0)

def cmd(obj):
    f.write((json.dumps(obj) + "\n").encode())
    while True:
        line = f.readline()
        if not line:
            return None
        msg = json.loads(line.decode())
        if "return" in msg or "error" in msg:
            if "error" in msg:
                print("[t2-gui] QMP error:", msg["error"], flush=True)
            return msg

def drain():
    while select.select([s], [], [], 0)[0]:
        if not f.readline():
            break

def log():
    try:
        return open(LOG, errors="replace").read()
    except FileNotFoundError:
        return ""

def wait_log(needle, timeout, after=0):
    t0 = time.time()
    while time.time() - t0 < timeout:
        drain()
        if needle in log()[after:]:
            return True
        if qemu.poll() is not None:
            return False
        time.sleep(1)
    return False

def send(events):
    cmd({"execute": "input-send-event", "arguments": {"events": events}})

def move(x, y):
    send([{"type": "abs", "data": {"axis": "x", "value": x * 32767 // (SW - 1)}},
          {"type": "abs", "data": {"axis": "y", "value": y * 32767 // (SH - 1)}}])

def click():
    send([{"type": "btn", "data": {"down": True, "button": "left"}}])
    time.sleep(0.2)
    send([{"type": "btn", "data": {"down": False, "button": "left"}}])

def key(spec):
    qs = spec.split("-") if spec not in ("minus",) else [spec]
    send([{"type": "key", "data": {"down": True, "key": {"type": "qcode", "data": q}}} for q in qs])
    time.sleep(0.12)
    send([{"type": "key", "data": {"down": False, "key": {"type": "qcode", "data": q}}} for q in reversed(qs)])
    time.sleep(0.12)

US = {" ": "spc", "-": "minus", ".": "dot", ",": "comma", "/": "slash", ";": "semicolon", "=": "equal",
      "!": "shift-1", "_": "shift-minus", ":": "shift-semicolon"}
for c in "abcdefghijklmnopqrstuvwxyz0123456789":
    US[c] = c
for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ":
    US[c] = "shift-" + c.lower()

cmd({"execute": "qmp_capabilities"}) if f.readline() else None
ok = wait_log(MARKER, TIMEOUT)
print(f"[t2-gui] marker {'seen' if ok else 'NOT seen'} after {int(time.time())} (log {len(log())} bytes)", flush=True)
time.sleep(6)
mark = 0
fails = 0
for raw in open(SCRIPT):
    line = raw.rstrip("\n")
    if not line.strip() or line.lstrip().startswith("#"):
        continue
    op, _, rest = line.strip().partition(" ")
    drain()
    if op == "move":
        x, y = rest.split(); move(int(x), int(y))
    elif op == "click":
        click()
    elif op == "clickat":
        x, y = rest.split(); move(int(x), int(y)); time.sleep(0.5); click()
    elif op == "key":
        key(rest.strip())
    elif op == "type":
        for ch in rest:
            if ch in US: key(US[ch])
            else: print(f"[t2-gui] no key for {ch!r}", flush=True)
    elif op == "wait":
        t = float(rest); end = time.time() + t
        while time.time() < end:
            drain(); time.sleep(0.5)
    elif op == "shot":
        path = os.path.abspath(os.path.join(WORK, rest.strip()))
        cmd({"execute": "screendump", "arguments": {"filename": path, "format": "png"}})
        time.sleep(1.0)
        print(f"[t2-gui] shot {path}", flush=True)
    elif op == "waitlog":
        parts = rest.rsplit(" ", 1)
        needle, t = (parts[0], int(parts[1])) if len(parts) == 2 and parts[1].isdigit() else (rest, 120)
        got = wait_log(needle, t, mark)
        print(f"[t2-gui] waitlog {needle!r}: {'ok' if got else 'TIMEOUT'}", flush=True)
        fails += 0 if got else 1
    elif op == "mark":
        mark = len(log())
    elif op == "expect":
        got = rest in log()[mark:]
        print(f"[t2-gui] expect {rest!r}: {'PASS' if got else 'FAIL'}", flush=True)
        fails += 0 if got else 1
    print(f"[t2-gui] {line}", flush=True)
    time.sleep(0.5)
try:
    cmd({"execute": "quit"})
except Exception:
    pass
try:
    qemu.wait(timeout=20)
except Exception:
    qemu.kill()
print(f"[t2-gui] done, {fails} failed step(s)", flush=True)
sys.exit(1 if fails or not ok else 0)
