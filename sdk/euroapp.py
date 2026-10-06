#!/usr/bin/env python3
"""euroapp — build, sign and verify EuroOS app packages (.eupkg).

A package is a plain STORED zip with three members: MANIFEST.toml, the binary,
and signature.ed25519 (a 64-byte Ed25519 signature over the manifest bytes).
The manifest carries the binary's SHA-256, so the signature covers the binary
through its hash. EuroOS installs a package only when the signature verifies
against a key the owner enrolled (`eupkg trust <file.pub>`) or the EuroOS
developer key, and the binary's hash matches.

  euroapp.py keygen  <out-prefix>                      writes <prefix>.key (32-byte seed) + <prefix>.pub
  euroapp.py build   --name N --version V --binary PATH --key KEY.key --out N-V.eupkg
                     [--title T] [--caps console,file,net,proc] [--profile full|utility|renderer|strict]
                     [--display none|x11] [--args "a b"] [--description D] [--license EUPL-1.2]
  euroapp.py verify  <pkg.eupkg> --pub KEY.pub
  euroapp.py info    <pkg.eupkg>

Only the Python `cryptography` package is needed (pip install cryptography).
"""
import argparse, hashlib, os, sys, zipfile
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
from cryptography.exceptions import InvalidSignature

VALID_CAPS = {"console", "file", "net", "proc", "window"}
VALID_PROFILES = {"full", "utility", "renderer", "strict", "euro"}
VALID_DISPLAY = {"none", "x11"}


def keygen(prefix):
    sk = Ed25519PrivateKey.generate()
    seed = sk.private_bytes_raw()
    pub = sk.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
    with open(prefix + ".key", "wb") as f:
        f.write(seed)
    os.chmod(prefix + ".key", 0o600)
    with open(prefix + ".pub", "wb") as f:
        f.write(pub)
    print(f"wrote {prefix}.key (keep private) and {prefix}.pub (give to users: eupkg trust {os.path.basename(prefix)}.pub)")


def manifest_text(a, sha):
    name_ok = a.name and all(c.isalnum() or c in "-_" for c in a.name)
    if not name_ok:
        sys.exit("name may only contain letters, digits, - and _")
    caps = [c for c in a.caps.split(",") if c] if a.caps else []
    bad = [c for c in caps if c not in VALID_CAPS]
    if bad:
        sys.exit(f"unknown capability {bad}; choose from {sorted(VALID_CAPS)}")
    if a.profile not in VALID_PROFILES or a.display not in VALID_DISPLAY:
        sys.exit("profile must be full|utility|renderer|strict|euro, display none|x11")
    if a.abi not in ("linux", "euro", "wasm"):
        sys.exit("abi must be linux (glibc program), euro (native static PIE) or wasm")
    if a.abi == "euro" and a.profile != "euro":
        a.profile = "euro"  # a native app always runs under the euro profile
    bin_name = os.path.basename(a.binary)
    lines = [
        "[package]",
        f'name = "{a.name}"',
        f'version = "{a.version}"',
        f'description = "{a.description}"',
        f'license = "{a.license}"',
        f'binary = "{bin_name}"',
        f'kind = "app"',
        f'title = "{a.title or a.name}"',
        f'caps = "{",".join(caps)}"',
        f'profile = "{a.profile}"',
        f'display = "{a.display}"',
        f'args = "{a.args}"',
        f'abi = "{a.abi}"',
        "",
        "[build]",
        f'binary_sha256 = "{sha}"',
        "reproducible = true",
        "",
    ]
    return "\n".join(lines), bin_name


def build(a):
    data = open(a.binary, "rb").read()
    sha = hashlib.sha256(data).hexdigest()
    text, bin_name = manifest_text(a, sha)
    seed = open(a.key, "rb").read()
    if len(seed) != 32:
        sys.exit("the key file must be a 32-byte Ed25519 seed (euroapp.py keygen)")
    sig = Ed25519PrivateKey.from_private_bytes(seed).sign(text.encode())
    out = a.out or f"{a.name}-{a.version}.eupkg"
    with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_STORED) as z:
        z.writestr("MANIFEST.toml", text)
        z.writestr(bin_name, data)
        z.writestr("signature.ed25519", sig)
    print(f"wrote {out}: {bin_name} ({len(data)} bytes, sha256 {sha[:16]}…), signed")


def read_pkg(path):
    with zipfile.ZipFile(path) as z:
        names = z.namelist()
        text = z.read("MANIFEST.toml").decode()
        sig = z.read("signature.ed25519")
        fields = {}
        for line in text.splitlines():
            if "=" in line and not line.startswith("["):
                k, v = line.split("=", 1)
                fields[k.strip()] = v.strip().strip('"')
        data = z.read(fields["binary"])
    return text, sig, fields, data, names


def verify(a):
    text, sig, fields, data, _ = read_pkg(a.pkg)
    pub = Ed25519PublicKey.from_public_bytes(open(a.pub, "rb").read())
    try:
        pub.verify(sig, text.encode())
    except InvalidSignature:
        sys.exit("signature INVALID for this public key")
    if hashlib.sha256(data).hexdigest() != fields.get("binary_sha256"):
        sys.exit("binary hash MISMATCH: the binary was changed after signing")
    print(f"{a.pkg}: signature OK, binary hash OK ({fields.get('name')} {fields.get('version')})")


def info(a):
    text, _, fields, data, names = read_pkg(a.pkg)
    print(f"{a.pkg}: members {names}")
    for k in ("name", "version", "title", "kind", "caps", "profile", "display", "abi", "args", "binary", "binary_sha256"):
        print(f"  {k:14} {fields.get(k, '')}")
    print(f"  binary bytes   {len(data)}")


def main():
    p = argparse.ArgumentParser(description="EuroOS app packager")
    sub = p.add_subparsers(dest="cmd", required=True)
    k = sub.add_parser("keygen"); k.add_argument("prefix")
    b = sub.add_parser("build")
    b.add_argument("--name", required=True); b.add_argument("--version", required=True)
    b.add_argument("--binary", required=True); b.add_argument("--key", required=True)
    b.add_argument("--out"); b.add_argument("--title", default="")
    b.add_argument("--caps", default="console,file"); b.add_argument("--profile", default="utility")
    b.add_argument("--display", default="none"); b.add_argument("--args", default="")
    b.add_argument("--abi", default="linux", help="linux (default), euro (native, no Linux library) or wasm")
    b.add_argument("--description", default=""); b.add_argument("--license", default="EUPL-1.2")
    v = sub.add_parser("verify"); v.add_argument("pkg"); v.add_argument("--pub", required=True)
    i = sub.add_parser("info"); i.add_argument("pkg")
    a = p.parse_args()
    {"keygen": lambda: keygen(a.prefix), "build": lambda: build(a), "verify": lambda: verify(a), "info": lambda: info(a)}[a.cmd]()


if __name__ == "__main__":
    main()
