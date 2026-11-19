#!/usr/bin/env python3
# scmd.py IMG PINS SMP OUTDIR "cmd1;cmd2" WAITFOR: boot an image under KVM (no NIC), run serial console commands, wait for a marker, stop. Team 11f-A.
import os, socket, subprocess, sys, time
img, pins, smp, out, cmds, want = sys.argv[1:7]
os.makedirs(out, exist_ok=True)
boot = os.path.join(out, "boot.img"); subprocess.run(["cp", "--sparse=always", img, boot], check=True)
sock = os.path.join(out, "serial.sock"); log = os.path.join(out, "serial.log")
q = subprocess.Popen(["taskset", "-c", pins, "qemu-system-x86_64", "-machine", "q35", "-m", "4096M", "-smp", smp, "-enable-kvm", "-cpu", "host",
    "-bios", "/usr/share/ovmf/OVMF.fd", "-drive", f"format=raw,file={boot}", "-chardev", f"socket,id=s0,path={sock},server=on,wait=off,logfile={log},logappend=off",
    "-serial", "chardev:s0", "-display", "none", "-no-reboot", "-nic", "none"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
open(os.path.join(out, "qemu.pid"), "w").write(str(q.pid))
s = None
for _ in range(100):
    try:
        s = socket.socket(socket.AF_UNIX); s.connect(sock); break
    except OSError:
        time.sleep(0.1)
s.setblocking(False); buf = b""
def pump(t):
    global buf
    end = time.time() + t
    while time.time() < end:
        try:
            d = s.recv(65536)
            if d: buf += d; continue
        except BlockingIOError: pass
        time.sleep(0.02)
end = time.time() + 90
while b"[scon-ready]" not in buf and time.time() < end: pump(0.5)
pump(5)
for c in cmds.split(";"):
    s.sendall((c + "\n").encode()); pump(1)
end = time.time() + 120
while want.encode() not in buf[-200000:] and time.time() < end: pump(0.5)
pump(1)
q.terminate(); q.wait(10)
print("done", want.encode() in buf)
