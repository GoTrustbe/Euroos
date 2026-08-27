#!/usr/bin/env python3
"""Build a EuroPack disk image: a dead-simple container for serving LARGE files
(the 485 MB chrome binary, .pak resources, big libraries) to EuroOS from a
second virtio disk, so they never have to be embedded in the kernel image.

Layout (little-endian):
  sector 0..:  header  "EUROPCK1" (8) | count u32 | reserved u32
               then per file: path[192] NUL-padded | offset u64 | size u64
               (entry = 208 B; header region is 4 KiB-aligned up to the first file)
  files:       raw bytes, each 4 KiB-aligned (fault-time reads are 4 KiB pages)

Usage: mkeuropack.py OUT.img FILE[:servedpath] ...
       (default served path = /pack/<basename>)
"""
import os, struct, sys

def main():
    if len(sys.argv) < 3:
        print(__doc__); sys.exit(1)
    out = sys.argv[1]
    specs = []
    for a in sys.argv[2:]:
        if ":" in a:
            src, served = a.split(":", 1)
        else:
            src, served = a, "/pack/" + os.path.basename(a)
        if len(served.encode()) > 191:
            sys.exit(f"served path too long: {served}")
        specs.append((src, served))

    ENTRY = 208
    header_bytes = 16 + ENTRY * len(specs)
    data_start = (header_bytes + 4095) & ~4095

    entries, blobs, off = [], [], data_start
    for src, served in specs:
        data = open(src, "rb").read()
        entries.append((served, off, len(data)))
        blobs.append((off, data))
        off = (off + len(data) + 4095) & ~4095

    with open(out, "wb") as f:
        f.write(b"EUROPCK1")
        f.write(struct.pack("<II", len(specs), 0))
        for served, o, sz in entries:
            p = served.encode()
            f.write(p + b"\0" * (192 - len(p)))
            f.write(struct.pack("<QQ", o, sz))
        for o, data in blobs:
            f.seek(o)
            f.write(data)
        # round the image up to a whole sector
        end = f.tell()
        f.seek((end + 511) & ~511 - 1 if end % 512 else end)
        f.truncate((end + 511) & ~511)

    total = os.path.getsize(out)
    print(f"{out}: {len(specs)} files, {total//1048576} MiB")
    for served, o, sz in entries:
        print(f"  {served}  @{o:#x}  {sz} B")

if __name__ == "__main__":
    main()
