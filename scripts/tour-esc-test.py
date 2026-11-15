#!/usr/bin/env python3
"""BUG-115: with virtio input the first-run tour must close on Esc.

Boots the PUBLIC image N times (KVM, virtio keyboard + tablet, no network). Once "[tour] first-run tour shown" is on
the serial console, Esc is sent over QMP after a random delay of 0..1.5 s (so it lands in every phase of the desktop
loop: the race of BUG-115 is between the tour's look at the ring and the loop's own key readers). PASS per boot =
"[tour] closed ... skipped" within 10 s. A tour that stays until a later click/key closes it is a FAIL.
usage: tour-esc-test.py IMAGE OUTDIR CPUS N [SEED]
"""
import json, os, random, socket, subprocess, sys, time
IMG, OUT, CPUS, N = sys.argv[1], os.path.abspath(sys.argv[2]), sys.argv[3], int(sys.argv[4])
rnd = random.Random(int(sys.argv[5]) if len(sys.argv) > 5 else 115)
os.makedirs(OUT, exist_ok=True)
fails = 0
for n in range(N):
    log = os.path.join(OUT, f"boot{n:03d}.log"); qmp = os.path.join(OUT, f"q{n}.sock")
    img = os.path.join(OUT, f"img{n}.img")
    try: os.unlink(qmp)
    except FileNotFoundError: pass
    p = subprocess.Popen(["taskset", "-c", CPUS, "qemu-system-x86_64", "-machine", "q35", "-enable-kvm", "-cpu", "host", "-m", "2048M", "-smp", "2",
        "-bios", "/usr/share/ovmf/OVMF.fd", "-display", "none", "-serial", f"file:{log}", "-nic", "none", "-no-reboot",
        "-qmp", f"unix:{qmp},server,nowait", "-drive", f"format=raw,file={IMG},snapshot=on",
        "-device", "virtio-keyboard-pci", "-device", "virtio-tablet-pci"], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    for _ in range(100):
        if os.path.exists(qmp): break
        time.sleep(0.1)
    s = socket.socket(socket.AF_UNIX); s.connect(qmp); f = s.makefile("rwb", buffering=0); f.readline()
    def cmd(o):
        f.write((json.dumps(o) + "\n").encode())
        while True:
            r = json.loads(f.readline().decode())
            if "return" in r or "error" in r: return r
    cmd({"execute": "qmp_capabilities"})
    def txt():
        try: return open(log, errors="replace").read()
        except FileNotFoundError: return ""
    t0 = time.time(); shown = False
    while time.time() - t0 < 300 and p.poll() is None:
        if "[tour] first-run tour shown" in txt(): shown = True; break
        time.sleep(0.05)
    verdict = "NOTOUR"
    if shown:
        d = rnd.uniform(0, 1.5); time.sleep(d)
        cmd({"execute": "send-key", "arguments": {"keys": [{"type": "qcode", "data": "esc"}], "hold-time": 80}})
        t1 = time.time(); ok = False
        while time.time() - t1 < 10:
            if "[tour] closed" in txt()[txt().find("[tour] first-run tour shown"):]: ok = True; break
            time.sleep(0.05)
        verdict = "PASS" if ok else "FAIL"
        if not ok: fails += 1
        print(f"[tour-esc] boot {n}: {verdict} (Esc {d:.2f}s after the tour appeared)", flush=True)
    else:
        fails += 1
        print(f"[tour-esc] boot {n}: no tour on the serial log", flush=True)
    try: cmd({"execute": "quit"})
    except Exception: pass
    try: p.wait(timeout=20)
    except Exception:
        if p.pid and p.pid > 1: p.kill()
    if verdict == "PASS":
        os.unlink(log)
print(f"[tour-esc] {N - fails}/{N} closed on Esc", flush=True)
sys.exit(1 if fails else 0)
