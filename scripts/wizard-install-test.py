#!/usr/bin/env python3
"""Guided-installer (wizard) end-to-end test on the public image profile.

Run 1: boot the LIVE image (AHCI) with a BLANK 4 GiB virtio-blk target. The
wizard opens on the desktop; this script drives it with QMP send-key only
(language, keyboard, disk, account, encryption, summary, install) and waits for
the executor's verdict in the serial log.
Run 2: boot the target disk ALONE. The lockscreen must offer the created user,
must NOT auto-login, must reject a wrong password and accept the typed one.

Usage: wizard-install-test.py [image] [outdir] [--standalone]
  --standalone  skip run 1 and boot the target.img already in outdir
"""
import json, os, socket, subprocess, sys, time

STANDALONE = "--standalone" in sys.argv
ARGS = [a for a in sys.argv[1:] if not a.startswith("--")]
IMG = ARGS[0] if len(ARGS) > 0 else "eurokernel.img"
OUT = ARGS[1] if len(ARGS) > 1 else "wizard-test"
OVMF = "/usr/share/ovmf/OVMF.fd"
os.makedirs(OUT, exist_ok=True)
TARGET = os.path.join(OUT, "target.img")
QMP = os.path.join(OUT, "qmp.sock")
HOST, USER, PASSWORD = "euro-test", "tester", "Sovereign-Pass-2026!"

QMAP = {" ": "spc", "\n": "ret", "-": "minus", "_": "shift-minus", ".": "dot", "!": "shift-1",
        "@": "shift-2", "#": "shift-3", "$": "shift-4", "%": "shift-5", "/": "slash", ",": "comma"}
for c in "abcdefghijklmnopqrstuvwxyz0123456789":
    QMAP[c] = c
for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ":
    QMAP[c] = "shift-" + c.lower()

if not STANDALONE:
    subprocess.run(["qemu-img", "create", "-f", "raw", TARGET, "4G"], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)


class Vm:
    def __init__(self, drives, serial):
        self.serial = serial
        try: os.unlink(QMP)
        except FileNotFoundError: pass
        self.p = subprocess.Popen([
            "qemu-system-x86_64", "-machine", "q35", "-cpu", "qemu64,+smep,+smap",
            "-m", "1536M", "-bios", OVMF, "-display", "none",
            "-serial", f"file:{serial}", "-qmp", f"unix:{QMP},server,nowait", "-no-reboot",
        ] + drives, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
        for _ in range(100):
            if os.path.exists(QMP): break
            time.sleep(0.2)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.connect(QMP); self.f = s.makefile("rwb", buffering=0)
        self.f.readline(); self.cmd({"execute": "qmp_capabilities"})

    def cmd(self, o):
        self.f.write((json.dumps(o) + "\n").encode()); return json.loads(self.f.readline().decode())

    def key(self, qcode):
        keys = [{"type": "qcode", "data": q} for q in qcode.split("-")]
        self.cmd({"execute": "send-key", "arguments": {"keys": keys}})
        time.sleep(0.12)

    def type(self, text):
        for ch in text:
            self.key(QMAP[ch])

    def shot(self, name):
        self.cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(os.path.join(OUT, name)), "format": "png"}})
        time.sleep(1.5)

    def log(self):
        try:
            return open(self.serial, errors="replace").read()
        except FileNotFoundError:
            return ""

    def wait_for(self, needle, timeout, after=0):
        t0 = time.time()
        while time.time() - t0 < timeout:
            txt = self.log()[after:]
            if needle in txt:
                return True
            if self.p.poll() is not None:
                return False
            time.sleep(1)
        return False

    def quit(self):
        try: self.cmd({"execute": "quit"})
        except Exception: pass
        try: self.p.wait(timeout=20)
        except Exception: self.p.kill()


results = []
def check(name, ok):
    results.append((name, ok)); print(f"[wizard-test] {'PASS' if ok else 'FAIL'}: {name}", flush=True)

# ── Run 1: live boot + wizard ─────────────────────────────────────────────
if STANDALONE:
    print("[wizard-test] RUN 1 skipped (--standalone)", flush=True)
else:
  print("[wizard-test] RUN 1: live image + blank 4 GiB virtio target", flush=True)
  vm = Vm([
      "-drive", f"format=raw,file={IMG}",
      "-drive", f"id=tgt,format=raw,file={TARGET},if=none",
      "-device", "virtio-blk-pci,drive=tgt,disable-modern=on",
  ], os.path.join(OUT, "serial-install.log"))
  up = vm.wait_for("[desktop] interactive loop started", 600)
  check("live desktop up", up)
  log = vm.log()
  check("install media read from own ESP", "install media read from own ESP" in log)
  check("live boot did not write the blank disk (RAM root)", "booting a RAM root" in log or "live root" not in log)
  time.sleep(3); vm.shot("1-welcome.png")
  vm.key("ret"); time.sleep(1.5)                 # Welcome -> Language
  vm.key("ret"); time.sleep(1.5)                 # Language (nl-BE) -> Keyboard
  vm.key("up"); vm.key("ret"); time.sleep(2)     # Keyboard: US QWERTY (what QMP sends) -> Disk
  vm.shot("2-disk.png")
  vm.key("ret"); time.sleep(1.5)                 # Disk (first blank) -> Account
  vm.type(HOST + "\n" + USER + "\n" + PASSWORD + "\n" + PASSWORD)
  time.sleep(1); vm.shot("3-account.png")
  vm.key("ret"); time.sleep(2)                   # Account -> Encryption (no TPM here)
  vm.shot("4-encryption.png")
  vm.key("ret"); time.sleep(2)                   # Encryption -> Summary
  vm.shot("5-summary.png")
  mark = len(vm.log())
  vm.key("ret")                                  # Install
  done = vm.wait_for("[q1x3]", 900, after=mark)
  log = vm.log()[mark:]
  check("executor ran", done)
  check("install verified OK ([q1x3] ... OK)", "[q1x3]" in log and "→ OK" in log)
  check("euroid store on target verified", "euroid-store=true" in log)
  check("wizard reported completion", "[wizard] Installation complete" in log)
  time.sleep(3); vm.shot("6-done.png")
  vm.quit()

# ── Run 2: standalone boot of the installation ────────────────────────────
print("[wizard-test] RUN 2: boot the installed disk alone", flush=True)
vm = Vm(["-drive", f"format=raw,file={TARGET}"], os.path.join(OUT, "serial-standalone.log"))
check("installed root mounted from disk", vm.wait_for("live root = the on-disk EuroFS", 600))
check("identity store loaded from disk", vm.wait_for("identity store loaded from disk", 300))
check("lockscreen reached", vm.wait_for("[ag-lock]", 300))
time.sleep(5); vm.shot("7-lockscreen.png")
log = vm.log()
check("no unattended auto-login on the installed system", "unattended boot" not in log and "login successful" not in log)
vm.type("wrong-pass\n")
check("wrong password rejected", vm.wait_for("[lock] login rejected for " + USER, 30))
time.sleep(1); vm.shot("8-rejected.png")
mark = len(vm.log())
vm.type(PASSWORD + "\n")
check("typed password signs in as the created user", vm.wait_for(f"[lock] login successful: {USER}", 60, after=mark))
check("desktop after login", vm.wait_for("[desktop] interactive loop started", 300))
time.sleep(20); vm.shot("9-desktop.png")  # TCG: give the first desktop frame time to be presented
vm.quit()

failed = [n for n, ok in results if not ok]
print(f"[wizard-test] {len(results) - len(failed)}/{len(results)} checks passed; screenshots + serial logs in {OUT}/", flush=True)
sys.exit(1 if failed else 0)
