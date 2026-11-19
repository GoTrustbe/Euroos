#!/usr/bin/env python3
"""slotwrite.py DISK KERNEL.efi KERNEL.efi.sig [SLOTNAME]: write KERNEL into the slot partition (default EuroSlot-A) of an installed disk image the way
write_image_to_slot() (kernel/src/update.rs) does: sector 0 = header (EUROSLT2, u64 length LE, sha256, Ed25519 signature, zero padding), image from sector 1.
Order as in the kernel: wipe the header, write the image, write the header last. The signature is the one the build made (not re-signed)."""
import hashlib, struct, sys
disk, kern, sigf = sys.argv[1:4]; want = sys.argv[4] if len(sys.argv) > 4 else "EuroSlot-A"
img = open(kern, "rb").read(); sig = open(sigf, "rb").read(); assert len(sig) == 64
f = open(disk, "r+b")
f.seek(512); h = f.read(512); assert h[:8] == b"EFI PART"
ent_lba, num, esz = struct.unpack_from("<QII", h, 72)[0], struct.unpack_from("<I", h, 80)[0], struct.unpack_from("<I", h, 84)[0]
f.seek(ent_lba * 512); arr = f.read(num * esz)
found = None
for i in range(num):
    e = arr[i * esz:(i + 1) * esz]
    if e[:16] == b"\0" * 16: continue
    first, last = struct.unpack_from("<QQ", e, 32); name = e[56:128].decode("utf-16le").rstrip("\0")
    print("partition", i, name, first, last)
    if name == want: found = (first, last)
assert found, "slot partition not found"
first, last = found; nsec = (len(img) + 511) // 512
assert nsec + 1 <= last - first + 1, "image larger than the slot"
hdr = b"EUROSLT2" + struct.pack("<Q", len(img)) + hashlib.sha256(img).digest() + sig
hdr = hdr.ljust(512, b"\0")
f.seek(first * 512); f.write(b"\0" * 512)
f.seek((first + 1) * 512); f.write(img + b"\0" * (nsec * 512 - len(img)))
f.seek(first * 512); f.write(hdr); f.flush()
f.seek(first * 512); assert f.read(512) == hdr
print("slot %s: %d B image, header sha256 %s" % (want, len(img), hashlib.sha256(img).hexdigest()[:16]))
