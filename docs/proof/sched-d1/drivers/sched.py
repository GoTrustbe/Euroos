#!/usr/bin/env python3
"""sched.py: boot one EuroOS image under KVM on the R630, pinned, no NIC, run serial
console commands and keep the serial log. Optional extra virtio disk, throttled.
usage: sched.py IMG WORKDIR --pin 0-3 [--smp 4] [--mem 4G] [--iops N] [--wait 0] CMD..."""
import argparse, os, subprocess, sys, time
sys.path.insert(0, "/srv/euroos/sched-runs/scripts")
import loadtest as lt

ap = argparse.ArgumentParser()
ap.add_argument("image"); ap.add_argument("workdir")
ap.add_argument("--pin", required=True); ap.add_argument("--smp", default="4"); ap.add_argument("--mem", default="4G")
ap.add_argument("--iops", type=int, default=0); ap.add_argument("--disk", default="")
ap.add_argument("--wait", type=float, default=0, help="seconds to idle after the console is up")
ap.add_argument("--timeout", type=int, default=1800); ap.add_argument("--boot-timeout", type=int, default=900)
ap.add_argument("cmds", nargs="*")
a = ap.parse_args()
os.makedirs(a.workdir, exist_ok=True)
sock = os.path.join(a.workdir, "ser.sock"); log = os.path.join(a.workdir, "serial.log")
for f in (sock,):
    if os.path.exists(f): os.unlink(f)
q = ["taskset", "-c", a.pin, "qemu-system-x86_64", "-machine", "q35", "-enable-kvm", "-cpu", "host", "-m", a.mem, "-smp", a.smp,
     "-bios", "/usr/share/ovmf/OVMF.fd", "-drive", f"format=raw,file={a.image},snapshot=on", "-nic", "none", "-display", "none",
     "-chardev", f"socket,id=ser0,path={sock},server=on,wait=off", "-serial", "chardev:ser0", "-no-reboot"]
if a.iops or a.disk:
    disk = a.disk or os.path.join(a.workdir, "vd.raw")
    if not a.disk:
        subprocess.run(["truncate", "-s", "1G", disk])
    thr = f",throttling.iops-total={a.iops}" if a.iops else ""
    q += ["-drive", f"if=none,id=vd0,file={disk},format=raw{thr}", "-device", "virtio-blk-pci,drive=vd0"]
p = subprocess.Popen(q, stdout=open(os.path.join(a.workdir, "qemu.out"), "w"), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
open(os.path.join(a.workdir, "qemu.pid"), "w").write(str(p.pid))
con = lt.SerialConsole(sock, log)
rc = 0
try:
    if not con.connect():
        print("no serial socket"); rc = 2
    else:
        t0 = time.time(); con.boot(a.boot_timeout); print(f"[sched.py] console up after {time.time()-t0:.0f} s", flush=True)
        if a.wait: con.idle(a.wait)
        for c in a.cmds:
            st, ms, lines = con.run(c, a.timeout)
            print(f"[sched.py] $ {c}: {st} {ms} ms", flush=True)
            for l in con.raw.splitlines():
                if l.startswith("[") and not l.startswith("[scon"):
                    print("  " + l[:400], flush=True)
            if st.startswith("CRASH"): rc = 3; break
except lt.CrashError as e:
    print("[sched.py] CRASH", e); rc = 3
finally:
    if p.pid > 1:
        p.kill(); p.wait()
    if os.path.exists(os.path.join(a.workdir, "vd.raw")): os.unlink(os.path.join(a.workdir, "vd.raw"))
sys.exit(rc)
