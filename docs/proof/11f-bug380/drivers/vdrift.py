#!/usr/bin/env python3
"""vdrift.py IMG WORKDIR PINS SMP MEM SECONDS [tcg]
Boots IMG (KVM, -nic none, or TCG), waits for the serial console, sends `uptime`
every 20 s and takes the vDSO clock it prints ("vdso clock: mono N ns, real N ns")
with the host clocks at the moment the line arrives. Prints the drift of the
guest MONOTONIC against the host CLOCK_MONOTONIC and of REALTIME against the
host wall clock over the run (BUG-380)."""
import os, re, socket, subprocess, sys, time
img, wd, pins, smp, mem, secs = sys.argv[1:7]
tcg = len(sys.argv) > 7 and sys.argv[7] == "tcg"
secs = int(secs)
os.makedirs(wd, exist_ok=True)
sp = os.path.join(wd, "serial.sock")
accel = ["-cpu", "qemu64,+smep,+smap"] if tcg else ["-enable-kvm", "-cpu", "host"]
args = ["taskset", "-c", pins, "qemu-system-x86_64", "-machine", "q35", "-m", mem, "-smp", smp] + accel + [
    "-bios", "/usr/share/ovmf/OVMF.fd", "-drive", "format=raw,file=%s,snapshot=on" % img, "-nic", "none",
    "-chardev", "socket,id=ser0,path=%s,server=on,wait=off,logfile=%s,logappend=off" % (sp, os.path.join(wd, "serial.log")),
    "-serial", "chardev:ser0", "-display", "none", "-no-reboot"]
env = dict(os.environ, TMPDIR=wd)
q = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=open(os.path.join(wd, "qemu.err"), "wb"), env=env)
open(os.path.join(wd, "qemu.pid"), "w").write("%d\n" % q.pid)
s = socket.socket(socket.AF_UNIX)
for _ in range(200):
    try:
        s.connect(sp); break
    except OSError:
        time.sleep(0.1)
s.settimeout(0.2)
buf = b""; ready = False; samples = []; rx = re.compile(rb"vdso clock: mono (\d+) ns, real (\d+) ns")
t_end_boot = time.time() + (5400 if tcg else 900)
next_send = 0; t_start = None
out = open(os.path.join(wd, "samples.txt"), "w", buffering=1)
try:
    while True:
        try:
            d = s.recv(65536)
            if not d:
                break
        except socket.timeout:
            d = b""
        now_m, now_r = time.monotonic(), time.time()
        buf += d
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            if not ready and (b"[scon-ready]" in line):
                ready = True; t_start = now_m; next_send = now_m
            m = rx.search(line)
            if m and ready:
                samples.append((now_m, now_r, int(m.group(1)), int(m.group(2))))
                out.write("%.6f %.6f %s %s\n" % (now_m, now_r, m.group(1).decode(), m.group(2).decode()))
        if not ready and time.time() > t_end_boot:
            print("no serial console"); break
        if ready and now_m >= next_send:
            s.sendall(b"uptime\n"); next_send = now_m + 20
        if ready and now_m - t_start > secs:
            break
        if q.poll() is not None:
            print("qemu exited"); break
finally:
    if q.poll() is None and q.pid > 1:
        q.kill()
    q.wait()
if len(samples) >= 2:
    (m0, r0, g0, gr0), (m1, r1, g1, gr1) = samples[0], samples[-1]
    dh = m1 - m0; dg = (g1 - g0) / 1e9; dr = (gr1 - gr0) / 1e9; dhr = r1 - r0
    # largest deviation of a sample from the straight line between first and last
    dev = max(abs(((g - g0) / 1e9) - (m - m0) * dg / dh) for m, _, g, _ in samples)
    print("samples %d over %.1f s host: MONOTONIC %+.1f ppm (%+.3f s), REALTIME %+.1f ppm (%+.3f s), host wall vs host mono %+.3f s; largest offset from the line %.3f s"
          % (len(samples), dh, (dg - dh) / dh * 1e6, dg - dh, (dr - dhr) / dhr * 1e6, dr - dhr, dhr - dh, dev))
else:
    print("samples %d: not measured" % len(samples))
