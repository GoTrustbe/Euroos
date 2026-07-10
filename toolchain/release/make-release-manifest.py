#!/usr/bin/env python3
# 3E-4: build a SIGNED release manifest that binds a source commit to the exact
# binary hashes of a release. Consumed by `euroupdate` (the same Ed25519 dev key
# that gates A/B updates) and published alongside the SBOM per tag.
#
# The manifest is the reproducible-build anchor: given the same commit + pinned
# toolchain, a third party rebuilds and MUST reproduce these hashes (the CI
# `repro-check` job double-builds and compares; full bit-for-bit determinism is
# tracked as remaining — see docs/CRA-CONFORMANCE.md).
#
# Usage: make-release-manifest.py --version v0.1.0 --commit <sha> \
#          --out release-manifest.json  FILE [FILE ...]
import argparse
import hashlib
import json
import os
import sys


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--version", required=True)
    ap.add_argument("--commit", default="")
    ap.add_argument("--toolchain", default="")
    ap.add_argument("--timestamp", default="")
    ap.add_argument("--out", default="release-manifest.json")
    ap.add_argument("--key", default="toolchain/eupkg/keys/dev.key")
    ap.add_argument("artifacts", nargs="+")
    args = ap.parse_args()

    artifacts = []
    for path in args.artifacts:
        if not os.path.isfile(path):
            sys.exit(f"artifact missing: {path}")
        artifacts.append(
            {"name": os.path.basename(path), "size": os.path.getsize(path), "sha256": sha256_file(path)}
        )

    manifest = {
        "format": "euroos-release/1",
        "version": args.version,
        "commit": args.commit,
        "toolchain": args.toolchain,
        "timestamp": args.timestamp,
        "artifacts": artifacts,
    }
    # Canonical, sorted, no-whitespace encoding → deterministic signing input.
    body = json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode()
    with open(args.out, "wb") as f:
        f.write(body)
    print(f"wrote {args.out} ({len(artifacts)} artifacts)")

    # Sign if the private key is available (CI provides it via a secret; the repo
    # ships only dev.pub, so unsigned generation stays possible for a dry run).
    if os.path.isfile(args.key):
        try:
            from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

            sk = Ed25519PrivateKey.from_private_bytes(open(args.key, "rb").read())
            open(args.out + ".sig", "wb").write(sk.sign(body))
            print(f"signed → {args.out}.sig (Ed25519, dev.key)")
        except Exception as e:  # noqa: BLE001
            print(f"warning: signing skipped ({e})")
    else:
        print(f"note: {args.key} absent — manifest not signed (dry run)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
