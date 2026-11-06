#!/usr/bin/env python3
"""Host-side HTTP peer for scripts/loadtest.py, run by QEMU SLIRP once per guest
connection (guestfwd=tcp:10.0.2.100:80-cmd:...): reads the request on stdin,
answers 200 with SIZE deterministic bytes on stdout, appends one line to HITLOG."""
import sys, time
size, hitlog = int(sys.argv[1]), sys.argv[2]
req = b""
while b"\r\n\r\n" not in req and len(req) < 65536:
    c = sys.stdin.buffer.read1(4096) if hasattr(sys.stdin.buffer, "read1") else sys.stdin.buffer.read(1)
    if not c:
        break
    req += c
body = bytes((i * 31) & 0xFF for i in range(size))
out = sys.stdout.buffer
out.write(b"HTTP/1.0 200 OK\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % len(body))
out.write(body)
out.flush()
with open(hitlog, "a") as f:
    f.write("%.3f %d %s\n" % (time.time(), len(body), req.split(b"\r\n")[0].decode(errors="replace")))
