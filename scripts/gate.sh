#!/usr/bin/env bash
# The stability gate (night plan N3.1): one command, one verdict.
#   1. host tests of every logic crate
#   2. dev build + QEMU boot: every self-test marker that ends in FAILED fails the
#      gate, the desktop must come up, no kernel fault
#   3. public image build + the installer wizard harness
# Usage: scripts/gate.sh [outdir]   (run as root or via sudo: the tree is root-owned)
set -u
cd "$(dirname "$0")/.."
OUT="${1:-/tmp/euroos-gate}"
mkdir -p "$OUT"
export PATH="/root/.cargo/bin:$PATH"
fail=0
step() { printf '\n==> %s\n' "$*"; }
verdict() { if [ "$1" = 0 ]; then echo "    PASS: $2"; else echo "    FAIL: $2"; fail=1; fi; }

step "host tests"
cargo test 2>&1 | tee "$OUT/host-tests.log" | grep -E '^test result|FAILED|error\[' | tail -5
grep -qE 'FAILED|error\[' "$OUT/host-tests.log"; verdict $(( ! $? )) "host tests"

step "dev build"
./scripts/build.sh > "$OUT/build-dev.log" 2>&1; verdict $? "dev build (eurokernel.img, self-test profile)"

step "dev boot in QEMU (markers)"
python3 - "$OUT" <<'PY'
import json, os, socket, subprocess, sys, time
out = sys.argv[1]; log = os.path.join(out, "dev-boot.log"); qmp = os.path.join(out, "qmp.sock")
tgt = os.path.join(out, "target.img")
subprocess.run(["qemu-img", "create", "-f", "raw", tgt, "4G"], check=True, stdout=subprocess.DEVNULL)
try: os.unlink(qmp)
except FileNotFoundError: pass
p = subprocess.Popen(["qemu-system-x86_64", "-machine", "q35", "-cpu", "qemu64,+smep,+smap", "-m", "3072M",
    "-bios", "/usr/share/ovmf/OVMF.fd", "-display", "none", "-serial", f"file:{log}",
    "-qmp", f"unix:{qmp},server,nowait", "-no-reboot", "-drive", "format=raw,file=eurokernel.img",
    "-drive", f"id=tgt,format=raw,file={tgt},if=none", "-device", "virtio-blk-pci,drive=tgt,disable-modern=on"],
    stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
for _ in range(100):
    if os.path.exists(qmp): break
    time.sleep(0.2)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(qmp); f = s.makefile("rwb", buffering=0)
def cmd(o): f.write((json.dumps(o)+"\n").encode()); return json.loads(f.readline().decode())
f.readline(); cmd({"execute": "qmp_capabilities"})
t0 = time.time(); txt = ""
while time.time() - t0 < 1800:
    txt = open(log, errors="replace").read() if os.path.exists(log) else ""
    if "[desktop] interactive loop started" in txt or "GENERAL PROTECTION" in txt or "PANIC" in txt or p.poll() is not None: break
    time.sleep(5)
try: cmd({"execute": "quit"})
except Exception: pass
p.wait(timeout=30)
txt = open(log, errors="replace").read()
failed = [l for l in txt.splitlines() if l.startswith("[") and "→ FAILED" in l]
desktop = "[desktop] interactive loop started" in txt
fault = "GENERAL PROTECTION FAULT" in txt or "PANIC" in txt
print(f"    boot {int(time.time()-t0)} s, desktop={desktop}, kernel-fault={fault}, FAILED markers={len(failed)}")
for l in failed[:20]: print("      " + l[:150])
sys.exit(0 if desktop and not fault and not failed else 1)
PY
verdict $? "dev boot: desktop reached, no kernel fault, no FAILED marker"

step "public image build"
./scripts/build.sh image > "$OUT/build-image.log" 2>&1; verdict $? "public image build"

step "installer wizard harness"
python3 scripts/wizard-install-test.py eurokernel.img "$OUT/wizard" > "$OUT/wizard.log" 2>&1; verdict $? "wizard install + standalone boot + login"
tail -1 "$OUT/wizard.log"

step "verdict"
if [ $fail = 0 ]; then echo "GATE: PASS"; else echo "GATE: FAIL (see $OUT)"; fi
exit $fail
