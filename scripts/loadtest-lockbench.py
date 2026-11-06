#!/usr/bin/env python3
"""J1 lockbench matrix under KVM (LOADTEST-R630).

Boots the `lockbench` image (scripts/build.sh lockbench) once per run, optionally
pins every vCPU thread to one host CPU (QMP query-cpus-fast, VM started with -S)
and binds guest memory with numactl, then parses the [lk-bench] lines.

Usage: lbmatrix.py IMAGE OUTDIR CONFIG [CONFIG ...] --runs N
  CONFIG = label:smp[:cpus[:mem]]   cpus = comma list, mem = numactl args (e.g. membind=0)
  e.g.  smp4:4   node0:8:0,2,4,6,8,10,12,14:membind=0   2nodes:8:0,2,4,6,1,3,5,7:interleave=0,1
Writes OUTDIR/lockbench.csv (one row per workload/mode/run) and keeps each serial log.
"""
import argparse, csv, json, os, re, shutil, socket, subprocess, time

LINE = re.compile(r"\[lk-bench\] (.+?)\s{2,}(J1|GLOBAL)\s+1 core:\s+(\d+) ops/s \| (\d+) cores?:\s+(\d+) ops/s \| scaling x(\d+)\.(\d+)")


def qmp(path):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    for _ in range(100):
        try:
            s.connect(path); break
        except OSError:
            time.sleep(0.1)
    f = s.makefile("rwb", buffering=0)
    f.readline()

    def cmd(c, **a):
        f.write((json.dumps({"execute": c, "arguments": a} if a else {"execute": c}) + "\n").encode())
        while True:
            r = json.loads(f.readline())
            if "return" in r or "error" in r:
                return r
    cmd("qmp_capabilities")
    return cmd


def one(image, out, label, smp, cpus, mem, run, timeout=300):
    img = os.path.join(out, f"tmp-{label}.img")
    shutil.copyfile(image, img)
    log = os.path.join(out, f"{label}-run{run}.log")
    q = os.path.join(out, f"qmp-{label}.sock")
    try: os.unlink(q)
    except FileNotFoundError: pass
    args = ["qemu-system-x86_64", "-machine", "q35", "-enable-kvm", "-cpu", "host", "-smp", str(smp),
            "-m", "2G", "-bios", "/usr/share/ovmf/OVMF.fd", "-drive", f"format=raw,file={img}",
            "-display", "none", "-serial", f"file:{log}", "-no-reboot", "-monitor", "none",
            "-qmp", f"unix:{q},server,nowait", "-S"]
    if mem:
        args = ["numactl", f"--{mem}"] + args
    if cpus:
        # every QEMU thread (iothreads, main loop) on the same set; vCPUs 1:1 below
        args = ["taskset", "-c", ",".join(map(str, cpus))] + args
    load = open("/proc/loadavg").read().split()[0]
    p = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    cmd = qmp(q)
    if cpus:
        for c in cmd("query-cpus-fast")["return"]:
            os.sched_setaffinity(c["thread-id"], {cpus[c["cpu-index"] % len(cpus)]})
    t0 = time.time()
    cmd("cont")
    try:
        p.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        p.kill(); p.wait()
    wall = time.time() - t0
    os.unlink(img)
    rows = []
    txt = open(log, errors="replace").read()
    for m in LINE.finditer(txt):
        rows.append(dict(config=label, smp=smp, run=run, workload=m.group(1).strip(), mode=m.group(2),
                         one_core=int(m.group(3)), ncores=int(m.group(4)), n_core=int(m.group(5)),
                         scaling=float(f"{m.group(6)}.{m.group(7)}"), host_load=load, wall_s=round(wall, 1)))
    ok = "correctness errors=0 order-inversions=0" in txt
    return rows, ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("image"); ap.add_argument("out"); ap.add_argument("configs", nargs="+")
    ap.add_argument("--runs", type=int, default=5)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    path = os.path.join(a.out, "lockbench.csv")
    new = not os.path.exists(path)
    with open(path, "a", newline="") as f:
        w = csv.DictWriter(f, fieldnames=["config", "smp", "run", "workload", "mode", "one_core", "ncores",
                                          "n_core", "scaling", "host_load", "wall_s"])
        if new: w.writeheader()
        for spec in a.configs:
            parts = spec.split(":")
            label, smp = parts[0], int(parts[1])
            cpus = [int(x) for x in parts[2].split(",")] if len(parts) > 2 and parts[2] else None
            mem = parts[3] if len(parts) > 3 else None
            for r in range(1, a.runs + 1):
                rows, ok = one(a.image, a.out, label, smp, cpus, mem, r)
                for row in rows: w.writerow(row)
                f.flush()
                print(f"{label} run {r}: {len(rows)} lines, bench-ok={ok}", flush=True)


if __name__ == "__main__":
    main()
