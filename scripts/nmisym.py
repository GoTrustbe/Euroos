#!/usr/bin/env python3
"""Name the addresses of the kernel's NMI probes in a serial log.

usage: nmisym.py KERNEL.map SERIAL.log [--all]

The link map (target/eurokernel.map of the image that ran) gives symbol ->
address at the preferred image base; the probe prints the runtime address of
nmi_handler, which gives the load base. For every probe: the interrupted RIP,
the task, and the kernel return addresses found on the stack, by name.
"""
import re, sys, bisect, subprocess

IMGBASE = 0x140000000


def load_map(path):
    syms = []
    for line in open(path, errors="replace"):
        p = line.split()
        if len(p) >= 3 and re.fullmatch(r"[0-9a-fA-F]{4}:[0-9a-fA-F]{8}", p[0]) and re.fullmatch(r"[0-9a-fA-F]{16}", p[2]):
            syms.append((int(p[2], 16) - IMGBASE, p[1]))
    syms.sort()
    return syms


def demangle(names):
    try:
        out = subprocess.run(["rustfilt"], input="\n".join(names), capture_output=True, text=True).stdout.split("\n")
        if len(out) >= len(names):
            return out[:len(names)]
    except OSError:
        pass
    res = []
    for n in names:
        m = re.findall(r"\d+([A-Za-z_][A-Za-z0-9_]*)", n)
        res.append("::".join(x for x in m if not re.fullmatch(r"h[0-9a-f]{16}", x))[-90:] or n[-90:])
    return res


def main():
    syms = load_map(sys.argv[1])
    rvas = [s[0] for s in syms]
    nmi_rva = next((r for r, n in syms if "nmi_handler" in n), None)
    if nmi_rva is None:
        sys.exit("no nmi_handler in the map")
    text = open(sys.argv[2], errors="replace").read()
    probes = text.split("========== NMI PROBE (wedge RIP capture) ==========")[1:]

    def name(addr, base):
        rva = addr - base
        i = bisect.bisect_right(rvas, rva) - 1
        if i < 0 or rva - rvas[i] > 0x40000:
            return None
        return syms[i][1], rva - rvas[i]

    for k, pr in enumerate(probes, 1):
        pr = pr.split("END NMI PROBE")[0]
        m = re.search(r"nmi_handler @ (0x[0-9a-f]+)", pr)
        r = re.search(r"interrupted RIP=(0x[0-9a-f]+) CS=(0x[0-9a-f]+).*RFLAGS=(0x[0-9a-f]+)", pr)
        t = re.search(r"\[nmi\] current task (\d+) (\"[^\"]*\") last-syscall=(\S+)", pr)
        if not (m and r):
            continue
        base = int(m.group(1), 16) - nmi_rva
        rip = int(r.group(1), 16)
        rows = [("RIP", rip)] + [("ret", int(a, 16)) for a in re.findall(r"\[nmi\]   (0x[0-9a-f]{16})", pr)]
        named = [(lab, a, name(a, base)) for lab, a in rows]
        named = [x for x in named if x[2]]
        dem = demangle([x[2][0] for x in named])
        iff = "IF=1" if int(r.group(3), 16) & 0x200 else "IF=0"
        print(f"probe {k}: CS={r.group(2)} {iff} task {t.group(1) if t else '?'} {t.group(2) if t else ''} last-syscall={t.group(3) if t else '?'}")
        for (lab, a, (n, off)), d in zip(named, dem):
            print(f"   {lab} {a:#x}  {d}+{off:#x}")
        c = re.search(r"CR2 \(address of the last page fault\) = (0x[0-9a-f]+)", pr)
        if c:
            print(f"   CR2 {c.group(1)}")
        z = re.search(r"t0 \(desktop loop\) was preempted at rip=(0x[0-9a-f]+) cs=(0x[0-9a-f]+) (.*)", pr)
        if z:
            nm = name(int(z.group(1), 16), base)
            where = (demangle([nm[0]])[0] + f"+{nm[1]:#x}") if nm else "(not kernel code)"
            print(f"   task 0 (desktop loop) preempted at {z.group(1)} cs={z.group(2)}  {where}  {z.group(3)}")
        ready = re.findall(r"\[census\] t(\d+) .* Ready", pr)
        print(f"   ready tasks: {' '.join(ready)}")


main()
