#!/usr/bin/env python3
# Sign a deterministic test "update image" with the EuroOS developer key (dev.key)
# so the kernel can PROVE — at boot, against the embedded dev.pub — that a genuine
# Ed25519 signature is accepted and any tampering is refused ([upd3]).
#
# The image + signature are public artifacts (they verify against the committed
# dev.pub) and are committed so the build is hermetic WITHOUT the private key.
# Re-run this only if the developer key is rotated.
import os
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

HERE = os.path.dirname(os.path.abspath(__file__))
KEY = HERE + "/keys/dev.key"
OUT = os.path.abspath(HERE + "/../../kernel/src/testdata")
os.makedirs(OUT, exist_ok=True)

# Deterministic 4 KiB test image (a stand-in for a signed A/B update payload).
header = b"EuroOS A/B update test image v1 -- signed with the EuroOS developer key\n"
body = bytes((i * 37 + 11) & 0xFF for i in range(4096 - len(header)))
img = header + body

seed = open(KEY, "rb").read()
sk = Ed25519PrivateKey.from_private_bytes(seed)
sig = sk.sign(img)
assert len(sig) == 64

open(OUT + "/update-test.img", "wb").write(img)
open(OUT + "/update-test.img.sig", "wb").write(sig)
print(f"wrote {OUT}/update-test.img ({len(img)} B) + .sig (64 B)")
