#!/usr/bin/env python3
"""Inbound TCP regression test (LOADTEST-R630 BUG-018): listen/accept over e1000e.

Boots IMAGE under KVM with an e1000e on SLIRP (hostfwd 127.0.0.1:PORT -> guest
:8080), then N times: a host client connects one second after the shell command
`tcpserve 8080` (listen + accept one connection + reply) was sent over scon. Each
round passes when the guest prints "connection ACCEPTED" and the client receives
the reply. Before the fix every accept() timed out: the central RX demux queued
the SYN where no listener looked.

usage: loadtest-accept.py IMAGE WORKDIR [N] [PORT] [CPUS]
"""
import os, shutil, socket, subprocess, sys, threading, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import loadtest as lt  # noqa: E402

img, wd = sys.argv[1], sys.argv[2]
n = int(sys.argv[3]) if len(sys.argv) > 3 else 6
port = int(sys.argv[4]) if len(sys.argv) > 4 else 18095
cpus = sys.argv[5] if len(sys.argv) > 5 else "0-3"
os.makedirs(wd, exist_ok=True)
shutil.copyfile(img, os.path.join(wd, "boot.img"))
sock = f"/tmp/euroos-accept-{os.getpid()}.sock"
q = subprocess.Popen(["taskset", "-c", cpus, "qemu-system-x86_64", "-machine", "q35", "-enable-kvm", "-cpu", "host",
                      "-smp", "4", "-m", "4G", "-bios", lt.OVMF, "-drive", f"format=raw,file={wd}/boot.img",
                      "-chardev", f"socket,id=s,path={sock},server=on,wait=off", "-serial", "chardev:s",
                      "-display", "none", "-no-reboot",
                      "-netdev", f"user,id=n0,restrict=on,hostfwd=tcp:127.0.0.1:{port}-:8080", "-device", "e1000e,netdev=n0"],
                     stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
con = lt.SerialConsole(sock, os.path.join(wd, "serial.log"))
res = []


def client():
    time.sleep(1.0)
    t0 = time.time()
    try:
        c = socket.create_connection(("127.0.0.1", port), timeout=60)
        c.sendall(b"GET / HTTP/1.0\r\n\r\n")
        c.settimeout(60)
        d = b""
        while True:
            x = c.recv(4096)
            if not x:
                break
            d += x
        res.append(("ok" if b"accept() works" in d else "bad", round(time.time() - t0, 2)))
    except OSError as e:
        res.append((f"err:{e}", round(time.time() - t0, 2)))


passed = 0
try:
    con.connect()
    con.boot(400)
    for i in range(n):
        th = threading.Thread(target=client)
        th.start()
        st, ms, lines = con.run("tcpserve 8080", timeout=300)
        th.join(90)
        acc = any("connection ACCEPTED" in l for l in lines)
        r = res[-1] if res else ("none", 0)
        ok = acc and r[0] == "ok"
        passed += ok
        print(f"round {i}: guest accepted={acc} client={r[0]} latency={r[1]}s shell={ms}ms", flush=True)
finally:
    q.terminate()
    q.wait()
    try:
        os.unlink(sock)
    except OSError:
        pass
print(f"[accept] {passed}/{n} inbound connections accepted and answered")
sys.exit(0 if passed == n else 1)
