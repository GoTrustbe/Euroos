#!/usr/bin/env python3
"""upd-drive.py DISK WD PINS PEERDIR: boot the installed disk (KVM, user-net restrict=on, guestfwd 10.0.2.100 -> local peer that serves
the candidate kernel + signature), run `euroupdate fetch http://10.0.2.100/cand.efi` (the production staging code of the INSTALLED kernel),
then `reboot`. Prints the console output of the commands."""
import os, re, socket, subprocess, sys, time
disk, wd, pins, peerdir = sys.argv[1:5]
os.makedirs(wd, exist_ok=True)
sp = os.path.join(wd, "serial.sock"); log = os.path.join(wd, "serial.log"); hits = os.path.join(wd, "peer-hits.log")
peer = os.path.join(os.path.dirname(os.path.abspath(__file__)), "upd-peer.py")
args = ["taskset", "-c", pins, "qemu-system-x86_64", "-machine", "q35", "-enable-kvm", "-cpu", "host", "-m", "2G", "-smp", "2",
        "-bios", "/usr/share/ovmf/OVMF.fd", "-display", "none", "-no-reboot",
        "-drive", "id=tgt,format=raw,file=%s,if=none" % disk, "-device", "virtio-blk-pci,drive=tgt,disable-modern=on",
        "-nic", "user,model=e1000e,restrict=on,guestfwd=tcp:10.0.2.100:80-cmd:python3 %s %s %s" % (peer, peerdir, hits),
        "-qmp", "unix:%s,server,nowait" % os.path.join(wd, "qmp.sock"),
        "-chardev", "socket,id=ser0,path=%s,server=on,wait=off,logfile=%s,logappend=off" % (sp, log), "-serial", "chardev:ser0"]
q = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=open(os.path.join(wd, "qemu.err"), "wb"), env=dict(os.environ, TMPDIR=wd))
open(os.path.join(wd, "qemu.pid"), "w").write("%d\n" % q.pid)
s = socket.socket(socket.AF_UNIX)
for _ in range(200):
    try: s.connect(sp); break
    except OSError: time.sleep(0.1)
s.settimeout(0.5)
buf = b""
def pump(secs, until=None):
    global buf
    end = time.time() + secs
    while time.time() < end:
        try: d = s.recv(65536)
        except socket.timeout: d = b""
        if d == b"" and q.poll() is not None: return False
        buf += d
        if until and re.search(until, buf): return True
    return False
def cmd(c, until, secs):
    global buf
    buf = b""; s.sendall(c.encode() + b"\n")
    echo = re.compile(rb"\[scon\] \$ " + re.escape(c.encode()))
    end = time.time() + secs; ok = False
    while time.time() < end:
        pump(1)
        m = echo.search(buf)
        if m and re.search(rb"\[scon-ready\]", buf[m.end():]): ok = True; break
    txt = buf.decode("utf-8", "replace")
    print("== %s (%s)" % (c, "ok" if ok else "TIMEOUT")); print("\n".join(l.rstrip() for l in txt.splitlines() if l.strip() and "[desk]" not in l and "[keylat" not in l)[:3500]); sys.stdout.flush()
    return ok, txt
QMAP = {" ": "spc", "-": "minus", "_": "shift-minus", ".": "dot", "!": "shift-1"}
for c in "abcdefghijklmnopqrstuvwxyz0123456789": QMAP[c] = c
for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ": QMAP[c] = "shift-" + c.lower()
import json
qs = socket.socket(socket.AF_UNIX)
for _ in range(100):
    try: qs.connect(os.path.join(wd, "qmp.sock")); break
    except OSError: time.sleep(0.1)
qf = qs.makefile("rwb", buffering=0); qf.readline()
def qmp(o):
    qf.write((json.dumps(o) + chr(10)).encode())
    while True:
        r = json.loads(qf.readline().decode())
        if "return" in r or "error" in r: return r
qmp({"execute": "qmp_capabilities"})
def key(k):
    qmp({"execute": "send-key", "arguments": {"keys": [{"type": "qcode", "data": x} for x in k.split("-")]}}); time.sleep(0.12)
try:
    if not pump(600, rb"phase desktop-build"): print("no desktop-build"); sys.exit(1)
    time.sleep(6)
    for ch in "Sovereign-Pass-2026!" + chr(10): key("ret" if ch == chr(10) else QMAP[ch])
    if not pump(60, rb"login successful"): print("no login")
    if not pump(300, rb"interactive loop started"): print("no desktop loop")
    if pump(8, rb"first-run tour shown"): key("esc"); pump(10, rb"closed")
    if not pump(60, rb"scon-ready"): print("no console"); sys.exit(1)
    time.sleep(2)
    cmd("euroupdate status", rb"watermark|last result|staged", 20)
    ok, txt = cmd("euroupdate fetch http://10.0.2.100/cand.efi", rb"staged|REFUSED|aborted|INVALID|not fetched", 600)
    cmd("euroupdate status", rb"watermark|last result|staged", 20)
    print("STAGED" if re.search(r"verified|staged for next boot|written to slot", txt) and "REFUSED" not in txt and "aborted" not in txt else "NOT-STAGED?")
    s.sendall(b"reboot\n"); pump(30)
finally:
    if q.poll() is None and q.pid > 1: q.kill()
    q.wait()
