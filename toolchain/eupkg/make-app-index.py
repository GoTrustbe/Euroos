#!/usr/bin/env python3
"""Build the SIGNED EuroOS app-store index (sprint N7).

    make-app-index.py --out DIR --base /apps --pub sdk/examples/keys/example-dev.pub PKG.eupkg...

Writes DIR/index.json and DIR/index.json.sig (Ed25519, the EuroOS key
toolchain/eupkg/keys/dev.key, root-only on the build server) and copies the
packages next to it. Every entry names the developer key (hex) that signed the
package: the index, signed by EuroOS, vouches for that key, so a kernel that
trusts the EuroOS key can verify the package without the user enrolling the
developer first. An entry's `file` is the repository path the kernel fetches
(`<base>/<file name>`).

    --testdata   write kernel/src/testdata/appindex.json(.sig) for the
                 kernel's [appstore] self-test (paths under /repo/, plus one
                 entry that names a WRONG key, which the kernel must refuse).
"""
import argparse, hashlib, json, os, shutil, zipfile

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

HERE = os.path.dirname(os.path.abspath(__file__))
KEY = os.path.join(HERE, "keys", "dev.key")


def manifest(path):
    with zipfile.ZipFile(path) as z:
        text = z.read("MANIFEST.toml").decode()
    out = {}
    for line in text.splitlines():
        if "=" in line and not line.startswith("["):
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip().strip('"')
    return out


def entry(path, base, pub_hex, key_override=None):
    m = manifest(path)
    data = open(path, "rb").read()
    return {
        "name": m["name"], "version": m["version"], "file": f"{base}/{os.path.basename(path)}",
        "deps": [], "title": m.get("title", m["name"]), "abi": m.get("abi", "linux"),
        "display": m.get("display", "none"), "caps": m.get("caps", ""),
        "summary": m.get("description", ""), "key": key_override or pub_hex,
        "sha256": hashlib.sha256(data).hexdigest(), "size": len(data),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--base", default="/apps")
    ap.add_argument("--pub", required=True, help="developer public key (32 bytes) that signed the packages")
    ap.add_argument("--testdata", action="store_true")
    ap.add_argument("pkgs", nargs="+")
    a = ap.parse_args()
    pub_hex = open(a.pub, "rb").read().hex()
    entries = [entry(p, a.base, pub_hex) for p in a.pkgs]
    if a.testdata:
        # One entry whose key is NOT the key that signed the package: the kernel
        # must refuse it even though the EuroOS-signed index lists it.
        wrong = entry(a.pkgs[0], a.base, pub_hex, key_override="11" * 32)
        wrong["name"] = "wrongkey"
        entries.append(wrong)
    index = json.dumps({"packages": entries}, separators=(",", ":")).encode()
    sk = Ed25519PrivateKey.from_private_bytes(open(KEY, "rb").read())
    os.makedirs(a.out, exist_ok=True)
    open(os.path.join(a.out, "index.json"), "wb").write(index)
    open(os.path.join(a.out, "index.json.sig"), "wb").write(sk.sign(index))
    if not a.testdata:
        for p in a.pkgs:
            shutil.copy(p, os.path.join(a.out, os.path.basename(p)))
    print(f"wrote {a.out}/index.json (+ .sig): {len(entries)} entries")


if __name__ == "__main__":
    main()
