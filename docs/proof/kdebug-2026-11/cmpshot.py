#!/usr/bin/env python3
"""Pixel comparison of two screenshots. usage: cmpshot.py A.png B.png [x0,y0,x1,y1 to ignore]
Prints the size, the number of differing pixels and their bounding box; with an
ignore box, also the number of differing pixels outside it."""
import sys
from PIL import Image, ImageChops

a = Image.open(sys.argv[1]).convert("RGB")
b = Image.open(sys.argv[2]).convert("RGB")
print(f"{sys.argv[1]}: {a.size[0]}x{a.size[1]}  {sys.argv[2]}: {b.size[0]}x{b.size[1]}")
if a.size != b.size:
    print("DIFFERENT SIZE")
    sys.exit(1)
d = ImageChops.difference(a, b).convert("L").point(lambda v: 255 if v else 0)
n = sum(1 for v in d.getdata() if v)
print(f"differing pixels: {n} of {a.size[0] * a.size[1]}; bounding box: {d.getbbox()}")
if len(sys.argv) > 3:
    x0, y0, x1, y1 = (int(v) for v in sys.argv[3].split(","))
    d.paste(0, (x0, y0, x1, y1))
    m = sum(1 for v in d.getdata() if v)
    print(f"outside ({x0},{y0})-({x1},{y1}): {m} differing pixels; bounding box: {d.getbbox()}")
    sys.exit(0 if m == 0 else 1)
sys.exit(0 if n == 0 else 1)
