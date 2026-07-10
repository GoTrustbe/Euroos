#!/usr/bin/env python3
# Build the SIGNED test repository index for the kernel [3e6] self-test
# (europkg::store — install/remove/upgrade on the content-addressed store).
#
# Like sign-test-image.py: the index + signature are public artifacts (they
# verify against the committed dev.pub) and are committed so the build stays
# hermetic WITHOUT the private key. Re-run only on key rotation or when
# hello-0.1.0.eupkg is rebuilt.
import json
import os

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.abspath(HERE + "/../../kernel/src/testdata")
os.makedirs(OUT, exist_ok=True)

index = json.dumps(
    {
        "packages": [
            {"name": "hello", "version": "0.1.0", "file": "/repo/hello-0.1.0.eupkg", "deps": []}
        ]
    },
    separators=(",", ":"),
).encode()

sk = Ed25519PrivateKey.from_private_bytes(open(HERE + "/keys/dev.key", "rb").read())
open(OUT + "/pkgindex.json", "wb").write(index)
open(OUT + "/pkgindex.json.sig", "wb").write(sk.sign(index))
print(f"wrote {OUT}/pkgindex.json (+ .sig)")
