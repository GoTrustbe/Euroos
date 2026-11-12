#!/usr/bin/env python3
"""montage.py OUT.png COLS X0,Y0,X1,Y1 IN.png [IN.png ...]: crop the same rectangle out of
each screenshot and tile the crops, COLS per row. 8-bit RGB PNGs, standard library only."""
import struct, sys, zlib

def read_png(path):
    d = open(path, "rb").read()
    pos, idat, w, h = 8, b"", 0, 0
    while pos < len(d):
        n, typ = struct.unpack(">I4s", d[pos:pos + 8])
        body = d[pos + 8:pos + 8 + n]
        if typ == b"IHDR":
            w, h, depth, ctype = struct.unpack(">IIBB", body[:10])
            assert depth == 8 and ctype == 2, "8-bit RGB only"
        elif typ == b"IDAT":
            idat += body
        pos += 12 + n
    raw = zlib.decompress(idat)
    stride = w * 3
    rows, prev = [], bytearray(stride)
    for y in range(h):
        f = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        if f == 1:
            for i in range(3, stride): line[i] = (line[i] + line[i - 3]) & 255
        elif f == 2:
            for i in range(stride): line[i] = (line[i] + prev[i]) & 255
        elif f == 3:
            for i in range(stride): line[i] = (line[i] + ((line[i - 3] if i >= 3 else 0) + prev[i]) // 2) & 255
        elif f == 4:
            for i in range(stride):
                a = line[i - 3] if i >= 3 else 0; b = prev[i]; c = prev[i - 3] if i >= 3 else 0
                p = a + b - c; pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(bytes(line)); prev = line
    return w, h, rows

def write_png(path, w, h, rows):
    def chunk(t, b): return struct.pack(">I", len(b)) + t + b + struct.pack(">I", zlib.crc32(t + b) & 0xffffffff)
    raw = b"".join(b"\0" + r for r in rows)
    open(path, "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))

out, cols = sys.argv[1], int(sys.argv[2])
geom = sys.argv[3].split("/")          # X0,Y0,X1,Y1[/N]: keep every Nth pixel
step = int(geom[1]) if len(geom) > 1 else 1
x0, y0, x1, y1 = [int(v) for v in geom[0].split(",")]
tiles = []
for p in sys.argv[4:]:
    w, h, rows = read_png(p)
    xa, xb, ya, yb = min(x0, w), min(x1, w), min(y0, h), min(y1, h)
    t = [rows[y][xa * 3:xb * 3] + b"\x40" * ((x1 - x0) - (xb - xa)) * 3 for y in range(ya, yb)]
    t += [b"\x40" * (x1 - x0) * 3] * ((y1 - y0) - len(t))
    if step > 1:
        t = [b"".join(r[i:i + 3] for i in range(0, len(r), 3 * step)) for r in t[::step]]
    tiles.append(t)
tw, th, gap = len(tiles[0][0]) // 3, len(tiles[0]), 6
nrows = (len(tiles) + cols - 1) // cols
W, H = cols * tw + (cols - 1) * gap, nrows * th + (nrows - 1) * gap
canvas = []
for r in range(nrows):
    for y in range(th):
        line = b""
        for c in range(cols):
            i = r * cols + c
            line += (tiles[i][y] if i < len(tiles) else b"\xff" * tw * 3) + (b"\xff" * gap * 3 if c < cols - 1 else b"")
        canvas.append(line)
    if r < nrows - 1:
        canvas += [b"\xff" * W * 3] * gap
write_png(out, W, H, canvas)
print(out, W, H)
