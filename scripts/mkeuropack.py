#!/usr/bin/env python3
"""Build a SIGNED EuroPack (v2) disk image: a container for serving LARGE files
(the 485 MB chrome binary, .pak resources, big libraries) to EuroOS from a second
virtio disk, so they never have to be embedded in the kernel image.

v2 is verified end to end. The manifest carries, per file, the root of a SHA-256
Merkle tree over its 4 KiB pages, and the manifest is Ed25519-signed with the
EuroOS developer key (toolchain/eupkg/keys/dev.key, the key that signs every
userland binary and the kernel image). The kernel refuses an unsigned pack,
recomputes every root from the leaf table, and checks every page against its
leaf as it is read. The exact layout and hashing rule live in
crates/europack/src/lib.rs; this script must match it byte for byte, and
`--check` re-parses the output with that rule and dev.pub.

Layout (little-endian):
  0    "EUROPCK2"                      8 B
  8    count u32 | flags u32           8 B
  16   salt[32]
  48   signature[64]   Ed25519 over bytes 0..48 ++ 112..(112+248*count)
  112  entries, count x 248 B: path[192] NUL-padded | data_off u64 | size u64
                               | leaves_off u64 | root[32]
  ...  leaf tables (4 KiB-aligned), then file data (4 KiB-aligned)

Usage: mkeuropack.py OUT.img FILE[:servedpath] ...   (default served path = /pack/<basename>)
       mkeuropack.py OUT.img --from-v1 OLD.img       (re-pack an unsigned v1 image)
       mkeuropack.py --check IMG.img                 (verify an image with dev.pub)
"""
import hashlib, os, struct, sys

HERE = os.path.dirname(os.path.abspath(__file__))
KEYS = os.path.join(HERE, "..", "toolchain", "eupkg", "keys")
PAGE, HEADER, ENTRY, PATHLEN = 4096, 112, 248, 192
MAGIC1, MAGIC2 = b"EUROPCK1", b"EUROPCK2"

def align(n): return (n + PAGE - 1) & ~(PAGE - 1)

def leaf(salt, page):
    h = hashlib.sha256(); h.update(salt); h.update(b"\x00"); h.update(page)
    if len(page) < PAGE: h.update(b"\x00" * (PAGE - len(page)))
    return h.digest()

def node(salt, l, r):
    h = hashlib.sha256(); h.update(salt); h.update(b"\x01"); h.update(l); h.update(r)
    return h.digest()

def root_of(salt, leaves):
    if not leaves: return b"\x00" * 32
    level = list(leaves)
    while len(level) > 1:
        nxt = []
        for i in range(0, len(level), 2):
            l = level[i]; r = level[i + 1] if i + 1 < len(level) else level[i]
            nxt.append(node(salt, l, r))
        level = nxt
    return level[0]

def load_key():
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
    seed = open(os.path.join(KEYS, "dev.key"), "rb").read()
    assert len(seed) == 32, "dev.key must be a 32-byte Ed25519 seed"
    sk = Ed25519PrivateKey.from_private_bytes(seed)
    pub = sk.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
    assert pub == open(os.path.join(KEYS, "dev.pub"), "rb").read(), "dev.key does not match dev.pub"
    return sk

def read_v1(path):
    """Yield (served_path, bytes) from an unsigned v1 image."""
    f = open(path, "rb")
    assert f.read(8) == MAGIC1, "not a v1 EuroPack"
    count, _ = struct.unpack("<II", f.read(8))
    ents = []
    for _ in range(count):
        e = f.read(208)
        p = e[:192].split(b"\0")[0].decode()
        o, sz = struct.unpack("<QQ", e[192:208])
        ents.append((p, o, sz))
    for p, o, sz in ents:
        f.seek(o); yield p, f.read(sz)

def build(out, files):
    """files: list of (served_path, bytes)."""
    count = len(files)
    for p, _ in files:
        if len(p.encode()) > PATHLEN - 1: sys.exit(f"served path too long: {p}")
    salt = os.urandom(32)
    off = align(HEADER + ENTRY * count)
    leaf_tables, leaf_offs = [], []
    for _, data in files:
        leaves = [leaf(salt, data[i:i + PAGE]) for i in range(0, len(data), PAGE)]
        leaf_tables.append(leaves); leaf_offs.append(off)
        off = align(off + 32 * len(leaves))
    data_offs = []
    for _, data in files:
        data_offs.append(off); off = align(off + len(data))
    sk = load_key()
    with open(out, "w+b") as f:
        f.write(MAGIC2); f.write(struct.pack("<II", count, 0)); f.write(salt); f.write(b"\0" * 64)
        for i, (p, data) in enumerate(files):
            pb = p.encode()
            f.write(pb + b"\0" * (PATHLEN - len(pb)))
            f.write(struct.pack("<QQQ", data_offs[i], len(data), leaf_offs[i]))
            f.write(root_of(salt, leaf_tables[i]))
        for i, leaves in enumerate(leaf_tables):
            f.seek(leaf_offs[i]); f.write(b"".join(leaves))
        for i, (_, data) in enumerate(files):
            f.seek(data_offs[i]); f.write(data)
        end = align(f.tell()); f.truncate(end)
        # sign: TBS = header without the signature field + all entries
        f.seek(0); hdr = f.read(HEADER); ents = f.read(ENTRY * count)
        sig = sk.sign(hdr[:48] + ents); assert len(sig) == 64
        f.seek(48); f.write(sig)
    for i, (p, data) in enumerate(files):
        print(f"  {p}  @{data_offs[i]:#x}  {len(data)} B  {len(leaf_tables[i])} pages")
    print(f"==> {out}: {count} files, {end} B, manifest Ed25519-signed (dev.key), per-page SHA-256 roots")

def check(path):
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
    pub = Ed25519PublicKey.from_public_bytes(open(os.path.join(KEYS, "dev.pub"), "rb").read())
    f = open(path, "rb"); hdr = f.read(HEADER)
    if hdr[:8] == MAGIC1: sys.exit("REFUSED: unsigned v1 pack")
    assert hdr[:8] == MAGIC2, "not a EuroPack"
    count = struct.unpack("<I", hdr[8:12])[0]; salt = hdr[16:48]; sig = hdr[48:112]
    ents = f.read(ENTRY * count)
    pub.verify(sig, hdr[:48] + ents)   # raises on failure
    bad = 0
    for i in range(count):
        e = ents[i * ENTRY:(i + 1) * ENTRY]
        p = e[:PATHLEN].split(b"\0")[0].decode()
        doff, size, loff = struct.unpack("<QQQ", e[192:216]); root = e[216:248]
        n = (size + PAGE - 1) // PAGE
        f.seek(loff); raw = f.read(32 * n); leaves = [raw[j * 32:(j + 1) * 32] for j in range(n)]
        ok_root = root_of(salt, leaves) == root
        f.seek(doff); pages_ok = 0
        for j in range(n):
            if leaf(salt, f.read(min(PAGE, size - j * PAGE))) == leaves[j]: pages_ok += 1
        st = "OK" if ok_root and pages_ok == n else "FAIL"
        if st == "FAIL": bad += 1
        print(f"  {st} {p}: root {'ok' if ok_root else 'MISMATCH'}, pages {pages_ok}/{n}")
    print(f"==> signature OK; {count - bad} of {count} files verify")
    sys.exit(1 if bad else 0)

def main():
    a = sys.argv[1:]
    if len(a) == 2 and a[0] == "--check": return check(a[1])
    if len(a) < 2: print(__doc__); sys.exit(1)
    out = a[0]
    if len(a) == 3 and a[1] == "--from-v1": return build(out, list(read_v1(a[2])))
    files = []
    for s in a[1:]:
        src, served = (s.split(":", 1) if ":" in s else (s, "/pack/" + os.path.basename(s)))
        files.append((served, open(src, "rb").read()))
    build(out, files)

if __name__ == "__main__":
    main()
