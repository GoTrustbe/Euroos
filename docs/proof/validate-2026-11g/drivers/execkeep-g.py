import glob, re
tot = 0
regions = {"exe": 0, "ld.so": 0, "arena": 0, "brk": 0, "stack": 0, "floor": 0, "vdso": 0}
rx = r"\b(exe|floor|vdso|arena|ld\.so|brk|stack) (0x[0-9a-f]+)"
for l in glob.glob("/dev/shm/g11/starts20-m/*/serial.log"):
    t = open(l, "rb").read().decode("utf-8", "replace")
    L = re.search(r"^\[aslr-layout\] launch .*", t, re.M)
    if not L:
        continue
    p = dict(re.findall(rx, L.group(0)))
    for e in re.findall(r"^\[aslr-layout\] exec .*", t, re.M):
        c = dict(re.findall(rx, e))
        tot += 1
        for k in regions:
            if c.get(k) == p.get(k):
                regions[k] += 1
print("## on: exec'd children (%d) that kept an address of the launched browser, per region: %s" % (tot, ", ".join("%s %d" % kv for kv in regions.items())))
