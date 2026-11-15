import re, sys, statistics as st
for path in sys.argv[1:]:
    tot=[]; kib=[]
    for l in open(path, errors="replace"):
        if "[fork] pid" not in l or "arena" not in l: continue
        m=re.search(r"arena(?: copy)? (\d+) ms, demand pages (\d+) ms", l)
        if not m: continue
        tot.append(int(m.group(1))+int(m.group(2)))
        c=re.search(r"(\d+) KiB copied", l); s=re.search(r"(\d+) KiB shared", l)
        kib.append(int(c.group(1)) if c else 0)
    if not tot: print(path, "no forks"); continue
    tot.sort()
    print(f"{path}: {len(tot)} forks, arena+demand ms median {st.median(tot)}, mean {st.mean(tot):.1f}, p90 {tot[int(len(tot)*0.9)]}, max {tot[-1]}; arena KiB copied per fork median {st.median(kib)}")
