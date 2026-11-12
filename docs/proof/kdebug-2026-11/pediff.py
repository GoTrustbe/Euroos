#!/usr/bin/env python3
"""Byte differences of two PE images, with the PE fields they fall in.
usage: pediff.py A.efi B.efi"""
import datetime, hashlib, struct, sys

ds = [open(p, "rb").read() for p in sys.argv[1:3]]
for p, d in zip(sys.argv[1:3], ds):
    print(f"{p.split('/')[-1]}: {len(d)} bytes sha256 {hashlib.sha256(d).hexdigest()}")
a, b = ds
if len(a) != len(b):
    print("different sizes")
    sys.exit(1)
diff = [i for i in range(len(a)) if a[i] != b[i]]
print(f"\ndiffering bytes: {len(diff)}")
for i in diff[:64]:
    print(f"  file offset {i:#x}: {a[i]:#04x} / {b[i]:#04x}")
pe = struct.unpack_from("<I", a, 0x3C)[0]
print(f"\nPE header at {pe:#x}; COFF TimeDateStamp at {pe + 8:#x}:")
for n, d in enumerate(ds, 1):
    ts = struct.unpack_from("<I", d, pe + 8)[0]
    when = datetime.datetime.fromtimestamp(ts, datetime.timezone.utc).strftime("%Y-%m-%d %H:%M:%S")
    print(f"  build {n}: {ts:#x} (as a time: {when} UTC)")
opt = pe + 24
rva, size = struct.unpack_from("<II", a, opt + 112 + 6 * 8)
nsec = struct.unpack_from("<H", a, pe + 6)[0]
optsz = struct.unpack_from("<H", a, pe + 20)[0]
sec = opt + optsz
for i in range(nsec):
    name = a[sec + i * 40: sec + i * 40 + 8].rstrip(b"\0").decode()
    vsize, va, rsize, raw = struct.unpack_from("<IIII", a, sec + i * 40 + 8)
    if va <= rva < va + max(vsize, rsize):
        off = raw + rva - va
        print(f"\ndebug directory: RVA {rva:#x}, {size} bytes, in {name}, file offset {off:#x}; its TimeDateStamp at {off + 4:#x}:")
        for n, d in enumerate(ds, 1):
            print(f"  build {n}: {struct.unpack_from('<I', d, off + 4)[0]:#x}")
