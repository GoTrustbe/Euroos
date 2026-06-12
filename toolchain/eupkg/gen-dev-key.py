#!/usr/bin/env python3
# Generate a fresh local Ed25519 developer keypair for building signed userland.
# Writes a 32-byte seed (dev.key, git-ignored) and the 32-byte public key
# (dev.pub, committed + embedded in the kernel). Mirrors what sign.py consumes.
import os
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization as s

KEYS = os.path.dirname(os.path.abspath(__file__)) + "/keys"
os.makedirs(KEYS, exist_ok=True)
sk = Ed25519PrivateKey.generate()
seed = sk.private_bytes(s.Encoding.Raw, s.PrivateFormat.Raw, s.NoEncryption())
pub = sk.public_key().public_bytes(s.Encoding.Raw, s.PublicFormat.Raw)
open(KEYS + "/dev.key", "wb").write(seed)
open(KEYS + "/dev.pub", "wb").write(pub)
os.chmod(KEYS + "/dev.key", 0o600)
print("wrote keys/dev.key (private, git-ignored) + keys/dev.pub (public)")
