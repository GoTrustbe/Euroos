#!/usr/bin/env python3
# The EuroUpdate delivery server (3E-2): serves the signed channel manifests +
# images built by make-repo.py. Plain HTTP is fine for the DEMO transport:
# authenticity/integrity come from the Ed25519 signatures over both manifest
# and image (the APT model); production runs the same layout behind HTTPS.
#
# Run on the QEMU host; the kernel reaches it via the SLIRP gateway
# 10.0.2.2:8722 ([3e2] boot self-test, `euroupdate check`).
import os
import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, "repo")
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8722

if not os.path.isdir(REPO):
    sys.exit("repo/ missing — run make-repo.py first")


class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=REPO, **kw)

    def log_message(self, fmt, *args):  # one concise line per request
        print(f"[updated] {self.client_address[0]} {fmt % args}")


print(f"[updated] EuroUpdate delivery server on 0.0.0.0:{PORT} (repo={REPO})")
ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
