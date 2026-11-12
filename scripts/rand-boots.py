#!/usr/bin/env python3
"""Boot a self-test image several times and compare what Linux-ABI programs got
as random bytes in each boot (BUG-080).

The [linux-rand] boot self-test runs userland/randprobe.c as several musl and
glibc processes and prints their lines. This script boots the image N times
(or reads serial logs it is given), and checks:

  * the [linux-rand] verdict of every boot, and one [entropy] line per boot;
  * AT_RANDOM, the stack guard, getrandom, /dev/urandom and /dev/random bytes:
    no value twice, over all processes of all boots;
  * AT_RANDOM is not the constant the kernel wrote before the fix;
  * getrandom output is not the old generator: splitmix64 over a counter is a
    bijection, so its first 8 bytes give the state away and the next 24 bytes
    of the same call can be predicted. The check does exactly that.

usage: scripts/rand-boots.py IMG [--boots N] [--accel kvm|tcg] [--cpu CPU]
                             [--mem 1G] [--smp 4] [--outdir DIR] [--timeout S]
                             [--pin CPUS] [--expect random|predictable]
       scripts/rand-boots.py --logs LOG [LOG ...] [--expect random|predictable]

--expect random (default): exit 0 when everything above holds.
--expect predictable: for the image from before the fix; exit 0 when the
script DID find equal or predicted values (the "before" half of the proof).

Each boot is its own QEMU with its own work directory under --outdir,
`-nic none`, the image opened with snapshot=on; it is stopped by its PID when
the verdict line is on the serial log or the timeout has passed.
"""
import argparse
import os
import re
import signal
import subprocess
import sys
import time

M64 = (1 << 64) - 1
C1, C2, GOLD = 0xBF58476D1CE4E5B9, 0x94D049BB133111EB, 0x9E3779B97F4A7C15
I1, I2 = pow(C1, -1, 1 << 64), pow(C2, -1, 1 << 64)
OLD_AT_RANDOM = bytes(0x5A ^ ((i * 31) & 0xFF) for i in range(16)).hex()
KEYS = ("at_random", "stack_guard", "getrandom", "getrandom2", "urandom", "random")
LINE = re.compile(r"^\[linux-rand\]\s+(\S+) randprobe: (.*)$")
VERDICT = "[linux-rand] getrandom, /dev/urandom, /dev/random and AT_RANDOM"


def mix(z):
    z = ((z ^ (z >> 30)) * C1) & M64
    z = ((z ^ (z >> 27)) * C2) & M64
    return z ^ (z >> 31)


def unmix(x):
    x ^= (x >> 31) ^ (x >> 62)
    x = (x * I2) & M64
    x ^= (x >> 27) ^ (x >> 54)
    x = (x * I1) & M64
    x ^= (x >> 30) ^ (x >> 60)
    return x


def predicted_words(hexbytes):
    """How many of the 8-byte words after the first follow from the first one
    under the old generator (state + 1 per word)."""
    b = bytes.fromhex(hexbytes)
    words = [int.from_bytes(b[i:i + 8], "little") for i in range(0, len(b) - 7, 8)]
    if len(words) < 2:
        return 0, 0
    z0 = unmix(words[0])
    hit = sum(1 for k, w in enumerate(words[1:], 1) if mix((z0 + k) & M64) == w)
    return hit, len(words) - 1


def parse(path):
    runs, verdict, entropy = {}, None, []
    with open(path, "rb") as f:
        for raw in f:
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            if line.startswith("[entropy]"):
                entropy.append(line)
            if line.startswith(VERDICT):
                verdict = "FAILED" if "FAILED" in line else ("OK" if "→ OK" in line else "?")
            m = LINE.match(line)
            if not m:
                continue
            run, rest = m.group(1), m.group(2)
            for tok in rest.split(" "):
                if "=" in tok:
                    k, v = tok.split("=", 1)
                    if k in KEYS and re.fullmatch(r"[0-9a-f]+", v):
                        runs.setdefault(run, {}).setdefault(k, v)
            if rest.startswith("stat "):
                runs.setdefault(run, {}).setdefault("stat", []).append(rest)
    return {"runs": runs, "verdict": verdict, "entropy": entropy}


def boot(args, i):
    d = os.path.join(args.outdir, f"boot-{i}")
    os.makedirs(d, exist_ok=True)
    log = os.path.join(d, "serial.log")
    open(log, "wb").close()
    cmd = []
    if args.pin:
        cmd += ["taskset", "-c", args.pin]
    cmd += ["qemu-system-x86_64", "-machine", "q35"]
    if args.accel == "kvm":
        cmd += ["-enable-kvm", "-cpu", args.cpu or "host"]
    else:
        cmd += ["-cpu", args.cpu or "qemu64,+smep,+smap"]
    cmd += ["-m", args.mem, "-smp", str(args.smp), "-bios", args.bios,
            "-drive", f"format=raw,file={args.image},snapshot=on", "-nic", "none",
            "-display", "none", "-serial", f"file:{log}", "-no-reboot"]
    env = dict(os.environ, TMPDIR=d)
    t0 = time.time()
    p = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=open(os.path.join(d, "qemu.out"), "wb"),
                         stderr=subprocess.STDOUT, env=env, cwd=d)
    why = "verdict"
    while True:
        with open(log, "rb") as f:
            if VERDICT.encode() in f.read():
                break
        if p.poll() is not None:
            why = "qemu exited"
            break
        if time.time() - t0 > args.timeout:
            why = "timeout"
            break
        time.sleep(0.5)
    time.sleep(0.5)
    if p.pid and p.pid > 1 and p.poll() is None:
        os.kill(p.pid, signal.SIGTERM)
        try:
            p.wait(timeout=20)
        except subprocess.TimeoutExpired:
            os.kill(p.pid, signal.SIGKILL)
            p.wait()
    print(f"boot {i}: {why} after {time.time() - t0:.1f} s", flush=True)
    return log


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("image", nargs="?")
    ap.add_argument("--logs", nargs="+")
    ap.add_argument("--boots", type=int, default=2)
    ap.add_argument("--accel", choices=("kvm", "tcg"), default="tcg")
    ap.add_argument("--cpu")
    ap.add_argument("--mem", default="1G")
    ap.add_argument("--smp", type=int, default=4)
    ap.add_argument("--outdir", default="rand-boots-out")
    ap.add_argument("--timeout", type=int, default=3600)
    ap.add_argument("--pin")
    ap.add_argument("--bios", default="/usr/share/ovmf/OVMF.fd")
    ap.add_argument("--expect", choices=("random", "predictable"), default="random")
    args = ap.parse_args()
    if not args.logs and not args.image:
        ap.error("an image or --logs is needed")
    if args.logs:
        logs = args.logs
    else:
        args.image = os.path.abspath(args.image)
        args.outdir = os.path.abspath(args.outdir)
        logs = [boot(args, i) for i in range(1, args.boots + 1)]

    boots = [parse(l) for l in logs]
    problems = []  # what an image with the fix must not show
    print(f"\n== {len(boots)} boot(s)")
    for i, b in enumerate(boots, 1):
        seeded = [e for e in b["entropy"] if "CSPRNG seeded" in e]
        status = [e for e in b["entropy"] if "CSPRNG seeded" in e or "NOT SEEDED" in e]
        print(f"boot {i}: [linux-rand] verdict {b['verdict']}; processes {sorted(b['runs'])}; "
              f"[entropy] status lines {len(status)} (seeded {len(seeded)})")
        if b["verdict"] != "OK":
            problems.append(f"boot {i}: [linux-rand] verdict is {b['verdict']}")
        if len(status) != 1 or len(seeded) != 1:
            problems.append(f"boot {i}: {len(status)} [entropy] status lines, {len(seeded)} seeded")
        if not b["runs"]:
            problems.append(f"boot {i}: no randprobe lines")

    for key in KEYS:
        seen = {}
        for i, b in enumerate(boots, 1):
            for run, vals in sorted(b["runs"].items()):
                if key in vals:
                    seen.setdefault(vals[key], []).append(f"boot{i}/{run}")
        total = sum(len(v) for v in seen.values())
        dup = {v: w for v, w in seen.items() if len(w) > 1}
        print(f"\n-- {key}: {total} values, {len(seen)} distinct")
        for i, b in enumerate(boots, 1):
            for run, vals in sorted(b["runs"].items()):
                if key in vals:
                    print(f"   boot{i}/{run:<12} {vals[key]}")
        for v, w in dup.items():
            problems.append(f"{key}: {v[:32]}… seen {len(w)} times ({', '.join(w)})")
        if key == "at_random" and OLD_AT_RANDOM in seen:
            problems.append(f"at_random: the pre-fix constant {OLD_AT_RANDOM} ({len(seen[OLD_AT_RANDOM])} processes)")

    print("\n-- getrandom against the old generator (splitmix64 over a counter): "
          "words 2..4 of a call predicted from its first word")
    for i, b in enumerate(boots, 1):
        for run, vals in sorted(b["runs"].items()):
            for key in ("getrandom", "getrandom2", "urandom", "random"):
                if key in vals:
                    hit, of = predicted_words(vals[key])
                    if hit:
                        problems.append(f"boot{i}/{run} {key}: {hit} of {of} words predicted from the first 8 bytes")
                    if key == "getrandom" or hit:
                        print(f"   boot{i}/{run:<12} {key:<10} predicted {hit} of {of}")

    print("\n-- stream statistics")
    for i, b in enumerate(boots, 1):
        for run, vals in sorted(b["runs"].items()):
            for s in vals.get("stat", []):
                print(f"   boot{i}/{run:<12} {s}")

    print()
    if problems:
        print(f"FOUND ({len(problems)}):")
        for p in problems:
            print(f"  - {p}")
    else:
        print("nothing equal, nothing predicted, every verdict OK")
    if args.expect == "random":
        print("RESULT:", "PASS (random)" if not problems else "FAIL")
        return 0 if not problems else 1
    weak = [p for p in problems if "predicted" in p or "seen" in p or "constant" in p]
    print("RESULT:", "PASS (the image is predictable, as expected of the one before the fix)" if weak else "FAIL (expected a predictable image)")
    return 0 if weak else 1


if __name__ == "__main__":
    sys.exit(main())
