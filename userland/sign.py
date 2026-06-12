#!/usr/bin/env python3
# EuroToolchain — sign userland binaries with the EuroOS Ed25519 developer key.
# Produces <file>.sig (64-byte detached signature) for each ELF passed in.
# The kernel embeds the matching public key (toolchain/eupkg/keys/dev.pub) and
# verifies these signatures before running a program (verify-before-execute).
import sys, os
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

HERE = os.path.dirname(os.path.abspath(__file__))
KEYS = os.path.join(HERE, "..", "toolchain", "eupkg", "keys")
seed = open(os.path.join(KEYS, "dev.key"), "rb").read()
assert len(seed) == 32, "dev.key must be a 32-byte Ed25519 seed"
sk = Ed25519PrivateKey.from_private_bytes(seed)

# Sanity: derived public key must match the one the kernel embeds.
pub = sk.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
expected = open(os.path.join(KEYS, "dev.pub"), "rb").read()
assert pub == expected, "dev.key does not match dev.pub — keypair mismatch!"

for path in sys.argv[1:]:
    data = open(path, "rb").read()
    sig = sk.sign(data)  # 64-byte detached Ed25519 signature over the whole file
    assert len(sig) == 64
    open(path + ".sig", "wb").write(sig)
    print(f"  signed {os.path.basename(path)} -> {os.path.basename(path)}.sig (Ed25519)")
