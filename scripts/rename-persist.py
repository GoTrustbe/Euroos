#!/usr/bin/env python3
"""Rename over an existing file on an INSTALLED system, across a restart (BUG-091).

Phase 0  install: the public image with a blank 4 GiB virtio disk, the wizard
         driven with the key sequence of scripts/wizard-install-test.py.
Phase 1  first boot of the installed disk, sign in. In the shell (serial
         console, the kernel's own file system calls): twelve files, then a new
         file moved over an existing one (twice), unlink + move, a save with a
         backup, a delete; every file read back, `scrub`. In the Terminal
         (typed, a Linux-ABI program): `renprobe save DIR 1`, `renprobe save
         DIR 2` (rename over an existing file, unlink + rename, unlink),
         `renprobe check DIR 2`, and the whole `renprobe` scenario. Then
         `reboot`.
Phase 2  second boot: every file read back, `renprobe check DIR 2`, `scrub`.
         Then one more move over an existing file and, five seconds later, the
         power is cut (this QEMU is killed).
Phase 3  third boot: the file of the power cut holds the old or the new text,
         nothing else; everything else as before; `scrub`.

usage: rename-persist.py --image PUB.img --workdir DIR --wizard scripts/wizard-install-test.py
                         [--kvm] [--pin CPUS] [--target INSTALLED.img]
The account of the installation is the one wizard-install-test.py creates; it is
read from that script. Every QEMU has `-nic none`, lives in --workdir and is
stopped through its own process handle.
"""
import argparse, json, os, re, shutil, socket, subprocess, sys, threading, time

ap = argparse.ArgumentParser()
ap.add_argument("--image", required=True)
ap.add_argument("--workdir", required=True)
ap.add_argument("--wizard", required=True, help="scripts/wizard-install-test.py (host name, user and password of the installation)")
ap.add_argument("--kvm", action="store_true")
ap.add_argument("--pin", default="")
ap.add_argument("--target", default="", help="an installed disk to use instead of installing")
a = ap.parse_args()
W = os.path.abspath(a.workdir); os.makedirs(W, exist_ok=True)
m = re.search(r'HOST, USER, PASSWORD = "([^"]+)", "([^"]+)", "([^"]+)"', open(a.wizard).read())
HOST, USER, PASSWORD = m.group(1), m.group(2), m.group(3)
SLOW = 1 if a.kvm else 4
OVMF = "/usr/share/ovmf/OVMF.fd"

QMAP = {" ": "spc", "\n": "ret", "-": "minus", "_": "shift-minus", ".": "dot", "!": "shift-1",
        "@": "shift-2", "#": "shift-3", "$": "shift-4", "%": "shift-5", "/": "slash", ",": "comma"}
for c in "abcdefghijklmnopqrstuvwxyz0123456789":
    QMAP[c] = c
for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ":
    QMAP[c] = "shift-" + c.lower()

results = []
def record(name, ok, detail=""):
    results.append((name, ok))
    print(f"[rename-persist] {'ok  ' if ok else 'FAIL'} {name}{' — ' + detail if detail else ''}", flush=True)
def note(text):
    print(f"[rename-persist]      {text}", flush=True)


class Guest:
    def __init__(self, name, drives):
        self.name = name
        self.lines = []
        self.logf = open(os.path.join(W, name + ".serial.log"), "w")
        qmp = os.path.join(W, name + ".qmp")
        if os.path.exists(qmp): os.unlink(qmp)
        acc = ["-enable-kvm", "-cpu", "host"] if a.kvm else ["-cpu", "qemu64,+smep,+smap"]
        cmd = ["qemu-system-x86_64", "-machine", "q35", *acc, "-m", "2048M", "-smp", "2", "-bios", OVMF,
               "-display", "none", "-vga", "std", "-serial", "stdio", "-no-reboot", "-nic", "none",
               "-qmp", f"unix:{qmp},server,nowait"] + drives
        if a.pin: cmd = ["taskset", "-c", a.pin] + cmd
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        threading.Thread(target=self._read, daemon=True).start()
        for _ in range(100):
            if os.path.exists(qmp): break
            time.sleep(0.2)
        s = socket.socket(socket.AF_UNIX); s.connect(qmp)
        self.f = s.makefile("rwb", buffering=0); self.f.readline()
        self.qmp({"execute": "qmp_capabilities"})

    def _read(self):
        for raw in self.p.stdout:
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            self.lines.append(line); self.logf.write(line + "\n"); self.logf.flush()

    def qmp(self, o):
        self.f.write((json.dumps(o) + "\n").encode())
        while True:
            r = json.loads(self.f.readline().decode())
            if "return" in r or "error" in r: return r

    def mark(self): return len(self.lines)

    def wait(self, needle, timeout, after=0):
        end = time.time() + timeout * SLOW
        while time.time() < end:
            if any(needle in l for l in self.lines[after:]): return True
            if self.p.poll() is not None: return any(needle in l for l in self.lines[after:])
            time.sleep(0.2)
        return False

    def key(self, qcode):
        self.qmp({"execute": "send-key", "arguments": {"keys": [{"type": "qcode", "data": q} for q in qcode.split("-")]}})
        time.sleep(0.12)

    def type(self, text):
        for ch in text: self.key(QMAP[ch])

    def shot(self, name):
        self.qmp({"execute": "screendump", "arguments": {"filename": os.path.join(W, name), "format": "png"}})
        time.sleep(1)

    def scon(self, cmd, timeout=120):
        """One shell command over the serial console; its output lines."""
        m = self.mark()
        self.p.stdin.write((cmd + "\n").encode()); self.p.stdin.flush()
        if cmd in ("reboot", "shutdown"): return []
        ok = self.wait("[scon-ready]", timeout, after=m)
        out = [l[7:] for l in self.lines[m:] if l.startswith("[scon] ") and not l.startswith("[scon] $ ")]
        if not ok: out.append("(no [scon-ready] after this command)")
        return out

    def term(self, cmd, timeout=120):
        """A command typed in the Terminal; the lines it printed (the kernel mirrors them as [e2e])."""
        m = self.mark()
        self.type(cmd + "\n")
        ok = self.wait("[e2e] [exit ", timeout, after=m)
        time.sleep(0.5)
        out = [l[6:] for l in self.lines[m:] if l.startswith("[e2e] ")]
        if not ok: out.append("(no exit line after this command)")
        return out

    def login(self):
        ok = self.wait("[boot-time] phase desktop-build", 300)
        time.sleep(5 * SLOW)
        m = self.mark()
        self.type(PASSWORD + "\n")
        ok = ok and self.wait(f"[lock] login successful: {USER}", 60, after=m)
        ok = ok and self.wait("[desktop] interactive loop started", 300)
        if self.wait("[tour] first-run tour shown", 5, after=m):
            m2 = self.mark(); self.key("esc"); self.wait("[tour] closed", 60, after=m2); time.sleep(2)
        ok = ok and self.wait("[scon-ready]", 60)
        time.sleep(3 * SLOW)
        return ok

    def crashed(self):
        pat = ("KERNEL PANIC", "[PANIC]", "panicked at", "DOUBLE FAULT", "GENERAL PROTECTION FAULT", "[idt] PAGE FAULT")
        return [l for l in self.lines if any(p in l for p in pat)][:3]

    def stop(self, kill=False):
        if self.p.poll() is None:
            if kill: self.p.kill()
            else: self.p.terminate()
        try: self.p.wait(timeout=20)
        except Exception: self.p.kill()
        time.sleep(0.5); self.logf.close()


TARGET = os.path.join(W, "target.img")
if a.target:
    shutil.copyfile(a.target, TARGET)
    note(f"phase 0 skipped: installed disk {a.target}")
else:
    # ── Phase 0: install ─────────────────────────────────────────────────────
    live = os.path.join(W, "live.img"); shutil.copyfile(a.image, live)
    subprocess.run(["qemu-img", "create", "-f", "raw", TARGET, "4G"], check=True, stdout=subprocess.DEVNULL)
    g = Guest("install", ["-drive", f"format=raw,file={live}", "-drive", f"id=tgt,format=raw,file={TARGET},if=none",
                          "-device", "virtio-blk-pci,drive=tgt,disable-modern=on"])
    up = g.wait("[desktop] interactive loop started", 600)
    time.sleep(3 * SLOW)
    g.key("ret"); time.sleep(1.5 * SLOW)             # Welcome -> Language
    g.key("ret"); time.sleep(1.5 * SLOW)             # Language -> Keyboard
    g.key("up"); g.key("ret"); time.sleep(2 * SLOW)  # Keyboard: US QWERTY (what QMP sends) -> Disk
    g.key("ret"); time.sleep(1.5 * SLOW)             # Disk (first blank) -> Account
    g.type(HOST + "\n" + USER + "\n" + PASSWORD + "\n" + PASSWORD); time.sleep(1 * SLOW)
    g.key("ret"); time.sleep(2 * SLOW)               # Account -> Encryption
    g.key("ret"); time.sleep(2 * SLOW)               # Encryption -> Summary
    m0 = g.mark(); g.key("ret")                      # Install
    done = g.wait("[q1x3]", 900, after=m0)
    txt = "\n".join(g.lines[m0:])
    record("phase 0: the wizard installed the system on a blank disk",
           up and done and "→ OK" in txt and g.wait("[wizard] Installation complete", 60, after=m0))
    time.sleep(3); g.stop(); os.unlink(live)

D = f"/home/{USER}/rp"       # native shell files
L = f"/home/{USER}/rl"       # the Linux-ABI program's documents
def text(i, ver): return f"native file {i} version {ver}"
expect = {}                  # path -> text, None = must not exist

def check_native(g, phase):
    bad = []
    for path, want in sorted(expect.items()):
        out = g.scon(f"cat {path}")
        got = out[0] if out else ""
        if want is None:
            if got.startswith("native file") or got.startswith("power"): bad.append(f"{path}: still there ({got!r})")
        elif got != want: bad.append(f"{path}: {got!r}, expected {want!r}")
    names = g.scon(f"ls {D}")
    stray = [n for n in names if ".tmp" in n or ".new" in n]
    record(f"{phase}: every file of the shell scenario reads back ({sum(1 for v in expect.values() if v is not None)} files, {sum(1 for v in expect.values() if v is None)} deleted or replaced names)",
           not bad and not stray, "; ".join(bad[:3] + [f"stray: {s.strip()}" for s in stray[:3]]))

def scrub(g, phase):
    out = g.scon("scrub", timeout=600)
    for l in out: note("scrub: " + l.strip())
    joined = "\n".join(out)
    clean = "superblock" in joined and "CORRUPT" not in joined and "UNRECOVERABLE" not in joined and "(no [scon-ready]" not in joined
    errs = re.search(r"errors?\s*:\s*(\d+)", joined)
    if errs and int(errs.group(1)) != 0: clean = False
    record(f"{phase}: scrub is clean", clean)

def linux_docs(g, phase, ver):
    out = g.term(f"renprobe check {L} {ver}")
    for l in out: note("terminal: " + l)
    record(f"{phase}: `renprobe check` in the Terminal: the documents hold version {ver}", any(l.strip() == "renprobe: PASS" for l in out))
    disk = {n: (g.scon(f"cat {L}/{n}") or [""])[0] for n in ("a.txt", "b.txt", "c.txt")}
    names = [n.strip() for n in g.scon(f"ls {L}")]
    note(f"shell view of {L}: a.txt={disk['a.txt']!r} b.txt={disk['b.txt']!r} c.txt={disk['c.txt']!r}; ls: {names}")
    ok = disk["a.txt"] == f"document a version {ver}" and disk["b.txt"] == f"document b version {ver}" \
        and not disk["c.txt"].startswith("document") and not any(".tmp" in n or ".new" in n for n in names)
    record(f"{phase}: the file system (shell `cat`, `ls`) has the same: a.txt and b.txt version {ver}, no c.txt, no temporary file", ok)

# ── Phase 1 ──────────────────────────────────────────────────────────────────
g = Guest("boot1", ["-drive", f"format=raw,file={TARGET}"])
record("phase 1: installed system boots from its disk, sign-in", g.login() and g.wait("live root = the on-disk EuroFS", 5))
note("whoami: " + " | ".join(g.scon("whoami")) + "; df: " + " | ".join(g.scon("df")))
for l in g.scon(f"mkdir {D}"): note("mkdir: " + l)
for i in range(12):
    g.scon(f"write {D}/n{i:02}.txt {text(i, 0)}"); expect[f"{D}/n{i:02}.txt"] = text(i, 0)
g.scon(f"write {D}/p.txt power version 0"); expect[f"{D}/p.txt"] = "power version 0"
check_native(g, "phase 1, before")
steps = []
def sh(cmd):
    out = g.scon(cmd); steps.append((cmd, out)); return out
# a new file moved over an existing one, twice
for ver in (1, 2):
    sh(f"write {D}/n03.tmp {text(3, ver)}"); sh(f"mv {D}/n03.tmp {D}/n03.txt")
expect[f"{D}/n03.txt"] = text(3, 2); expect[f"{D}/n03.tmp"] = None
# unlink + move
sh(f"write {D}/n05.new {text(5, 1)}"); sh(f"rm {D}/n05.txt"); sh(f"mv {D}/n05.new {D}/n05.txt")
expect[f"{D}/n05.txt"] = text(5, 1); expect[f"{D}/n05.new"] = None
# a save with a backup, three rounds
for ver in (1, 2, 3):
    sh(f"write {D}/n07.new {text(7, ver)}")
    if ver > 1: sh(f"rm {D}/n07.bak")
    sh(f"mv {D}/n07.txt {D}/n07.bak"); sh(f"mv {D}/n07.new {D}/n07.txt")
expect[f"{D}/n07.txt"] = text(7, 3); expect[f"{D}/n07.bak"] = text(7, 2); expect[f"{D}/n07.new"] = None
# a delete
sh(f"rm {D}/n09.txt"); expect[f"{D}/n09.txt"] = None
for cmd, out in steps: note(f"$ {cmd}  ->  {' | '.join(o.strip() for o in out)}")
record("phase 1: mv over an existing file, rm + mv, save with a backup, rm: no command refused",
       not any("mv:" in o or "rm:" in o or "error" in o.lower() for _, out in steps for o in out))
check_native(g, "phase 1, after")
# the Linux-ABI program, typed in the Terminal
for l in g.scon(f"mkdir {L}"): note("mkdir: " + l)
for ver in (1, 2):
    out = g.term(f"renprobe save {L} {ver}")
    for l in out: note("terminal: " + l)
    record(f"phase 1: `renprobe save {L} {ver}` in the Terminal ({'creates three documents' if ver == 1 else 'rename over a.txt, unlink + rename of b.txt, unlink of c.txt'})",
           any(l.strip() == "renprobe: PASS" for l in out))
linux_docs(g, "phase 1", 2)
out = g.term(f"renprobe /home/{USER}/rs", timeout=300)
fails = [l for l in out if " FAILED " in l]
for l in fails[:6] + out[-3:]: note("terminal: " + l)
record("phase 1: the whole `renprobe` scenario in the Terminal (held descriptors, mappings)", any(l.strip() == "renprobe: PASS" for l in out),
       f"{sum(1 for l in out if l.strip().endswith(' ok'))} checks ok, {len(fails)} failed")
g.shot("phase1-terminal.png")
scrub(g, "phase 1")
record("phase 1: no fault line on the serial log", not g.crashed(), " | ".join(g.crashed()))
g.scon("reboot")
try: g.p.wait(timeout=60 * SLOW)
except Exception: pass
record("phase 1: `reboot` leaves the guest (QEMU -no-reboot exits)", g.p.poll() is not None)
g.stop()

# ── Phase 2 ──────────────────────────────────────────────────────────────────
g = Guest("boot2", ["-drive", f"format=raw,file={TARGET}"])
record("phase 2: second boot, sign-in", g.login())
check_native(g, "phase 2, after the restart")
linux_docs(g, "phase 2, after the restart", 2)
scrub(g, "phase 2")
record("phase 2: no fault line on the serial log", not g.crashed(), " | ".join(g.crashed()))
# power cut five seconds after a move over an existing file
g.scon(f"write {D}/p.tmp power version 1")
out = g.scon(f"mv {D}/p.tmp {D}/p.txt"); note("$ mv p.tmp p.txt -> " + " | ".join(out))
note("cat p.txt before the power cut: " + " | ".join(g.scon(f"cat {D}/p.txt")))
time.sleep(5)
g.stop(kill=True)
note("power cut: QEMU killed 5 s after the mv")

# ── Phase 3 ──────────────────────────────────────────────────────────────────
g = Guest("boot3", ["-drive", f"format=raw,file={TARGET}"])
record("phase 3: boot after the power cut, sign-in", g.login())
got = (g.scon(f"cat {D}/p.txt") or [""])[0]
record("phase 3: the file moved over before the power cut holds the old or the new text", got in ("power version 0", "power version 1"), repr(got))
expect.pop(f"{D}/p.txt")
check_native(g, "phase 3")
scrub(g, "phase 3")
record("phase 3: no fault line on the serial log", not g.crashed(), " | ".join(g.crashed()))
g.shot("phase3-desktop.png")
g.scon("shutdown")
try: g.p.wait(timeout=60 * SLOW)
except Exception: pass
g.stop()
if not a.target: pass
os.unlink(TARGET)

failed = [n for n, ok in results if not ok]
print(f"[rename-persist] {len(results) - len(failed)}/{len(results)} checks ok", flush=True)
for n in failed: print(f"[rename-persist] FAILED: {n}", flush=True)
sys.exit(1 if failed else 0)
