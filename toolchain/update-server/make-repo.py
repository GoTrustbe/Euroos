#!/usr/bin/env python3
# Build the EuroUpdate delivery-server repository (3E-2): Ed25519-SIGNED channel
# manifests + Ed25519-signed images, hash-pinned (the APT security model —
# signed metadata AND signed payload, so a hostile mirror/MITM can at worst
# serve nothing, never a tampered image).
#
# Layout produced under repo/:
#   channel/stable.json(.sig)   version 2 → the kernel (running v1) stages it
#   channel/old.json(.sig)      version 1 → the kernel reports up-to-date
#   channel/evil.json(.sig)     FORGED signature → the kernel must REFUSE
#   images/euroos-v2.img(.sig)  deterministic signed test payload
#
# Signing uses the developer key (keys/dev.key, git-ignored); everything the
# repo serves verifies against the committed dev.pub baked into the kernel.
import hashlib
import json
import os

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

HERE = os.path.dirname(os.path.abspath(__file__))
KEY = os.path.join(HERE, "..", "eupkg", "keys", "dev.key")
REPO = os.path.join(HERE, "repo")

sk = Ed25519PrivateKey.from_private_bytes(open(KEY, "rb").read())

os.makedirs(REPO + "/channel", exist_ok=True)
os.makedirs(REPO + "/images", exist_ok=True)

# Deterministic v2 test image (a stand-in for a signed A/B update payload).
header = b"EuroOS A/B update image v2 -- delivered by the EuroUpdate channel server\n"
body = bytes((i * 41 + 7) & 0xFF for i in range(4096 - len(header)))
img = header + body
open(REPO + "/images/euroos-v2.img", "wb").write(img)
open(REPO + "/images/euroos-v2.img.sig", "wb").write(sk.sign(img))

def manifest(channel: str, version: int) -> bytes:
    return json.dumps(
        {
            "channel": channel,
            "version": version,
            "image": "/images/euroos-v2.img",
            "sha256": hashlib.sha256(img).hexdigest(),
        },
        separators=(",", ":"),
    ).encode()

# stable: version 2 → newer than the running v1 → the kernel stages it.
m = manifest("stable", 2)
open(REPO + "/channel/stable.json", "wb").write(m)
open(REPO + "/channel/stable.json.sig", "wb").write(sk.sign(m))

# old: version 1 → the kernel reports "already up to date".
m = manifest("old", 1)
open(REPO + "/channel/old.json", "wb").write(m)
open(REPO + "/channel/old.json.sig", "wb").write(sk.sign(m))

# evil: valid-LOOKING manifest with a signature over DIFFERENT bytes → the
# kernel must refuse the channel before fetching anything.
m = manifest("evil", 99)
open(REPO + "/channel/evil.json", "wb").write(m)
open(REPO + "/channel/evil.json.sig", "wb").write(sk.sign(m + b"tampered"))

print(f"repo built under {REPO} (stable=v2, old=v1, evil=forged-sig)")
