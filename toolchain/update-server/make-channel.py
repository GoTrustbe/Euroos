#!/usr/bin/env python3
# Build the PUBLIC EuroUpdate channel from a real kernel build: an Ed25519-signed
# channel manifest + the Ed25519-signed kernel image, hash-pinned (signed metadata
# AND signed payload: a hostile mirror can at worst serve nothing).
#
#   make-channel.py --kernel target/x86_64-unknown-uefi/release/eurokernel.efi \
#                   --version 20260908 --channel stable --out /tmp/euroos-release/update
#
# Layout under --out (served at https://euro-os.eu/update/):
#   channel/<channel>.json(.sig)   {"channel","version","image","sha256","built"}
#   images/euroos-<version>.efi(.sig)
# The kernel compares `version` with its EUROOS_BUILD_VERSION (YYYYMMDD) and only
# stages a strictly newer one. Signing key: toolchain/eupkg/keys/dev.key (the
# public half is baked into the kernel as crypto::EUROOS_PUBKEY).
import argparse, hashlib, json, os, time
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
HERE = os.path.dirname(os.path.abspath(__file__))
ap = argparse.ArgumentParser()
ap.add_argument("--kernel", required=True)
ap.add_argument("--version", required=True, type=int, help="YYYYMMDD, must match EUROOS_BUILD_VERSION of --kernel")
ap.add_argument("--channel", default="stable")
ap.add_argument("--out", required=True)
ap.add_argument("--key", default=os.path.join(HERE, "..", "eupkg", "keys", "dev.key"))
ap.add_argument("--base", default="/update", help="URL prefix the manifest's image path uses")
ap.add_argument("--image-name", default=None, help="file name under images/ (default euroos-<version>.efi); the rescue channel uses a stable name")
ap.add_argument("--expires-days", type=int, default=120, help="manifest validity; the kernel refuses an expired manifest (fail closed)")
a = ap.parse_args()
# The version is the clients' monotonic anti-rollback watermark: a typo such as
# 202609088 would make every later real release "older". Insist on a real date.
if not (20000101 <= a.version <= 29991231) or time.strptime(str(a.version), "%Y%m%d") is None:
    raise SystemExit(f"--version {a.version} is not a YYYYMMDD date")
sk = Ed25519PrivateKey.from_private_bytes(open(a.key, "rb").read())
img = open(a.kernel, "rb").read()
os.makedirs(f"{a.out}/channel", exist_ok=True); os.makedirs(f"{a.out}/images", exist_ok=True)
name = a.image_name or f"euroos-{a.version}.efi"
open(f"{a.out}/images/{name}", "wb").write(img)
open(f"{a.out}/images/{name}.sig", "wb").write(sk.sign(img))
m = json.dumps({"channel": a.channel, "version": a.version, "image": f"{a.base}/images/{name}",
                "sha256": hashlib.sha256(img).hexdigest(), "size": len(img),
                "built": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "expires": int(time.time()) + a.expires_days * 86400}, separators=(",", ":")).encode()
open(f"{a.out}/channel/{a.channel}.json", "wb").write(m)
open(f"{a.out}/channel/{a.channel}.json.sig", "wb").write(sk.sign(m))
print(f"channel {a.channel}: version {a.version}, image {name} ({len(img)//1048576} MiB), sha256 {hashlib.sha256(img).hexdigest()[:16]}…")
