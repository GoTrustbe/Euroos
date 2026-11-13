#!/usr/bin/env python3
"""One guest connection of a QEMU `guestfwd=...-cmd:` forward, to a TCP service on this host.

QEMU's user-mode network (restrict=on) starts this program once per guest connection to the
forwarded address and gives it the connection as stdin/stdout. It connects to HOST:PORT
(default 127.0.0.1) and copies both ways until the server closes; ending the program closes
the guest's connection.

  -netdev user,id=n0,restrict=on,guestfwd=tcp:10.0.2.2:6631-cmd:python3 scripts/slirp-guestfwd-bridge.py 6631

Why not `guestfwd=tcp:10.0.2.2:6631-tcp:127.0.0.1:6631`: QEMU opens ONE host connection at
start-up (and refuses to start if nothing listens yet) and sends every guest connection over it
in turn; a server that answers one request per connection and then closes (the mock IPP and
eSCL servers) never sees the second connection, and the guest waits for it for ever.
"""
import os
import socket
import sys
import threading

port = int(sys.argv[1])
host = sys.argv[2] if len(sys.argv) > 2 else "127.0.0.1"
srv = socket.create_connection((host, port), timeout=10)
srv.settimeout(None)


def guest_to_server():
    try:
        while True:
            data = os.read(0, 65536)
            if not data:
                break
            srv.sendall(data)
    except OSError:
        pass
    try:
        srv.shutdown(socket.SHUT_WR)
    except OSError:
        pass


threading.Thread(target=guest_to_server, daemon=True).start()
try:
    while True:
        data = srv.recv(65536)
        if not data:
            break
        os.write(1, data)
except OSError:
    pass
