#!/usr/bin/env python3
"""Two restores of one snapshot must not hand out the same random bytes (BUG-089).

1. Boot IMAGE (a qcow2 overlay over it, so QEMU can keep an internal
   snapshot) to the desktop, ask the serial console for `random` twice, then
   `savevm snap` and quit.
2. Wait --gap seconds, then start a NEW QEMU from that snapshot (-loadvm snap)
   --restores times. Each one: `random` over the serial console, then quit
   without saving.
3. Compare: the bytes of the restores must differ from each other and from
   what the original printed after the snapshot would have printed (the
   original is gone, so: from each other). Before the fix they are equal:
   the DRBG state is in the snapshot.

Every QEMU has -nic none, its own QMP socket in --workdir and is stopped
through its own process handle. With --vmgenid (default) the machine has
`-device vmgenid,guid=auto`: a new QEMU gets a new generation ID, as a
hypervisor gives a restored or cloned guest. --no-vmgenid tests the clock
check alone.

usage: restore-reseed.py --image PUB.img --workdir DIR [--kvm] [--pin CPUS]
                         [--restores 2] [--gap 20] [--no-vmgenid]
"""
import argparse, json, os, re, socket, subprocess, sys, threading, time

ap = argparse.ArgumentParser()
ap.add_argument("--image", required=True)
ap.add_argument("--workdir", required=True)
ap.add_argument("--kvm", action="store_true")
ap.add_argument("--pin", default="")
ap.add_argument("--restores", type=int, default=2)
ap.add_argument("--gap", type=int, default=20)
ap.add_argument("--no-vmgenid", action="store_true")
a = ap.parse_args()
W = os.path.abspath(a.workdir); os.makedirs(W, exist_ok=True)
SLOW = 1 if a.kvm else 4
OVMF = "/usr/share/ovmf/OVMF.fd"
DISK = os.path.join(W, "disk.qcow2")
RAND = re.compile(r"^\[scon\] random: ([0-9a-f]{64})")

def log(t): print(f"[restore-reseed] {t}", flush=True)

class Guest:
    def __init__(self, name, extra):
        self.lines = []
        self.logf = open(os.path.join(W, name + ".serial.log"), "w")
        qmp = os.path.join(W, name + ".qmp")
        if os.path.exists(qmp): os.unlink(qmp)
        acc = ["-enable-kvm", "-cpu", "host"] if a.kvm else ["-cpu", "qemu64,+smep,+smap"]
        gen = [] if a.no_vmgenid else ["-device", "vmgenid,guid=auto"]
        cmd = ["qemu-system-x86_64", "-machine", "q35", *acc, "-m", "2048M", "-smp", "2", "-bios", OVMF,
               "-display", "none", "-vga", "std", "-serial", "stdio", "-no-reboot", "-nic", "none",
               "-drive", f"file={DISK},format=qcow2", *gen, "-qmp", f"unix:{qmp},server,nowait"] + extra
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

    def hmp(self, cmd):
        return self.qmp({"execute": "human-monitor-command", "arguments": {"command-line": cmd}})

    def wait(self, needle, timeout, after=0):
        end = time.time() + timeout * SLOW
        while time.time() < end:
            if any(needle in l for l in self.lines[after:]): return True
            if self.p.poll() is not None: break
            time.sleep(0.2)
        return any(needle in l for l in self.lines[after:])

    def random(self):
        m = len(self.lines)
        self.p.stdin.write(b"random\n"); self.p.stdin.flush()
        self.wait("[scon-ready]", 60, after=m + 1)
        for l in self.lines[m:]:
            r = RAND.match(l)
            if r: return r.group(1), l[len("[scon] "):]
        return None, "(no random line)"

    def stop(self):
        if self.p.poll() is None:
            try: self.qmp({"execute": "quit"})
            except Exception: pass
        try: self.p.wait(timeout=30)
        except Exception: self.p.kill(); self.p.wait()
        self.logf.close()

if os.path.exists(DISK): os.unlink(DISK)
subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-b", os.path.abspath(a.image), "-F", "raw", DISK], check=True)

results = []
def record(name, ok, detail=""):
    results.append((name, ok))
    log(f"{'ok  ' if ok else 'FAIL'} {name}{' — ' + detail if detail else ''}")

g = Guest("original", [])
up = g.wait("[desktop] interactive loop started", 300) and g.wait("[scon-ready]", 60)
time.sleep(3 * SLOW)
r1, l1 = g.random(); r2, l2 = g.random()
log(f"original: {l1}"); log(f"original: {l2}")
for l in g.lines:
    if l.startswith("[entropy]"): log("original serial: " + l)
record("the original boots and gives two different values", up and r1 is not None and r2 is not None and r1 != r2)
g.qmp({"execute": "stop"})
sv = g.hmp("savevm snap")
log(f"savevm: {sv}")
g.stop()
log(f"waiting {a.gap} s before the restores")
time.sleep(a.gap)

got = []
for i in range(a.restores):
    g = Guest(f"restore{i + 1}", ["-loadvm", "snap"])
    time.sleep(2 * SLOW)
    r, l = g.random()
    log(f"restore {i + 1}: {l}")
    for x in g.lines:
        if x.startswith("[entropy]"): log(f"restore {i + 1} serial: " + x)
    got.append(r)
    g.stop()

ok_all = all(r is not None for r in got)
record(f"every restore answered `random` ({sum(r is not None for r in got)}/{a.restores})", ok_all)
distinct = len(set(got)) == len(got) and ok_all
record("the restores of one snapshot give different bytes", distinct, " vs ".join((r or "-")[:16] for r in got))
record("no restore repeats a value the original gave before the snapshot", ok_all and not ({r1, r2} & set(got)))
failed = [n for n, ok in results if not ok]
log(f"{len(results) - len(failed)}/{len(results)} checks ok")
os.unlink(DISK)
sys.exit(1 if failed else 0)
