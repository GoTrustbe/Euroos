#!/usr/bin/env python3
# host side of guestfwd=tcp:10.0.2.100:80-cmd:...: serves DIR/<name> for GET /<name>, one line per request in LOG
import sys, os, time
d, log = sys.argv[1], sys.argv[2]
req = b""
while b"\r\n\r\n" not in req and len(req) < 65536:
    c = sys.stdin.buffer.read1(4096)
    if not c: break
    req += c
line = req.split(b"\r\n")[0].decode(errors="replace")
parts = line.split()
name = os.path.basename(parts[1]) if len(parts) > 1 else ""
p = os.path.join(d, name)
out = sys.stdout.buffer
if name and os.path.isfile(p):
    body = open(p, "rb").read()
    out.write(b"HTTP/1.0 200 OK\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % len(body)); out.write(body)
else:
    out.write(b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
out.flush()
open(log, "a").write("%.3f %s\n" % (time.time(), line))
