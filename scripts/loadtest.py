#!/usr/bin/env python3
"""EuroOS load/functional test harness — host-side serial driver.

Drives the EuroOS shell over COM1 using the in-kernel **serial console** (`scon`):
the host streams command lines into a bidirectional QEMU serial socket and parses the
`[scon] …` / `[scon-ready]` framing. The loop lives here (the EuroOS shell has none).

Honours the two ground rules from the load-test plan:
  • persistent, disk-backed drives — **never `-snapshot`** (else persistence tests pass
    for the wrong reason);
  • the full serial stream is captured to a timestamped log for every run.

Features: a `SerialConsole` driver with **crash detection** (panic / red-screen / double
fault in the stream, or serial-silence past a timeout) and a **CSV results log**
(one row per command: id, command, status, elapsed_ms, output_lines).

Usage:
  python3 scripts/loadtest.py [--image eurokernel.img] [--scenario smoke|users|fs]
                              [--users N] [--disks "64M,2G"] [--csv results.csv]
"""
import argparse, csv, os, re, socket, subprocess, sys, time

OVMF = next((c for c in ("/usr/share/ovmf/OVMF.fd", "/usr/share/OVMF/OVMF.fd",
                         "/usr/share/edk2-ovmf/x64/OVMF.fd") if os.path.exists(c)), None)
# Match ONLY real crash output, not EuroOS's benign boot self-tests (which deliberately
# print "breakpoint exception handled", "[isolation] … TERMINATED", "[j3-fault]", etc.).
CRASH_RE = re.compile(r"KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT")
READY = "[scon-ready]"


def parse_size(s):
    s = s.strip().upper()
    mult = {"M": 1024**2, "G": 1024**3, "K": 1024}.get(s[-1:], 1)
    return int(s[:-1] if s[-1] in "MGK" else s) * mult


# A command can return cleanly to the prompt yet still have FAILED its intent
# (e.g. `eurousers add` denied with EPERM). `status=ok` only means "the shell came
# back"; this regex marks such rows `err` so a run can't pass for the wrong reason.
# Tuned to known hard-failure signatures — NOT generic words like "error(s): 0".
ERR_RE = re.compile(
    r"\b(?:EPERM|EACCES|EEXIST|ENOENT|ENOSPC|EINVAL|EROFS|ELOOP|ENOTDIR|EBUSY|EIO)\b"
    r"|requires CAP_|Permission denied|not in the sudoers"
    r"|No such file|command not found|: not found"
    r"|too short|too long|too weak|already exists|not permitted"     # policy rejections
    r"|invalid |must be |rejected|refused|denied"
    r"|error:|cannot |unable to |failed to ")


class CrashError(Exception):
    pass


class SerialConsole:
    """Bidirectional COM1 driver over a QEMU unix-socket chardev."""

    def __init__(self, sock_path, logfile):
        self.sock_path = sock_path
        self.log = open(logfile, "ab", buffering=0)
        self.buf = ""
        self.sock = None

    def connect(self, tries=120):
        for _ in range(tries):
            try:
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.connect(self.sock_path)
                s.settimeout(1.0)
                self.sock = s
                return True
            except OSError:
                time.sleep(0.5)
        return False

    def _pump(self):
        """Read whatever is available; tee to the log; scan for crashes."""
        try:
            data = self.sock.recv(65536)
        except socket.timeout:
            return ""
        except OSError:
            return ""
        if data:
            self.log.write(data)
            text = data.decode(errors="replace")
            self.buf += text
            if CRASH_RE.search(text):
                raise CrashError(text.strip().splitlines()[-1] if text.strip() else "crash")
            return text
        return ""

    def wait_for(self, needle, timeout=60):
        """Wait until `needle` appears; raise CrashError on crash or silence."""
        deadline = time.time() + timeout
        last_data = time.time()
        while time.time() < deadline:
            chunk = self._pump()
            if chunk:
                last_data = time.time()
            if needle in self.buf:
                return True
            # Silence detection: no serial output for a long stretch = likely hang.
            if time.time() - last_data > timeout:
                raise CrashError("serial silent past timeout (hang?)")
            if not chunk:
                time.sleep(0.05)
        raise CrashError(f"timeout waiting for {needle!r}")

    def boot(self, timeout=400):
        """Wait for the first [scon-ready] (serial console up)."""
        self.wait_for(READY, timeout)
        self.buf = ""

    def run(self, cmd, timeout=60):
        """Send a command, collect its `[scon] …` output up to the next [scon-ready].
        Returns (status, elapsed_ms, output_lines)."""
        self.buf = ""
        self.sock.sendall((cmd + "\n").encode())
        t0 = time.time()
        try:
            self.wait_for(READY, timeout)
            status = "ok"
        except CrashError as e:
            return ("CRASH:" + str(e)[:60], int((time.time() - t0) * 1000), [])
        elapsed = int((time.time() - t0) * 1000)
        lines = [l[len("[scon] "):] for l in self.buf.splitlines()
                 if l.startswith("[scon] ") and not l.startswith("[scon] $ ")]
        # Intent check: the shell returned, but did the command actually succeed?
        if any(ERR_RE.search(l) for l in lines):
            status = "err"
        return (status, elapsed, lines)


def launch_qemu(image, disks, sock_path, serial_log):
    args = ["qemu-system-x86_64", "-machine", "q35", "-m", "1024",
            "-cpu", "qemu64,+smep,+smap", "-bios", OVMF,
            "-drive", f"format=raw,file={image}",          # NOT -snapshot (persistent)
            "-chardev", f"socket,id=ser0,path={sock_path},server=on,wait=off",
            "-serial", "chardev:ser0", "-display", "none", "-no-reboot"]
    for i, path in enumerate(disks):
        args += ["-drive", f"format=raw,file={path},if=none,id=ld{i}",
                 "-device", f"virtio-blk-pci,drive=ld{i},disable-modern=on"]
    return subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)


# ── scenarios ──
# Each item is either a bare command string (checked only by the ERR_RE backstop) or a
# (command, expect_substring) tuple. `expect` is a POSITIVE assertion: the substring MUST
# appear in the command's output, else the row is `err`. Positive assertions are the real
# §7 baseline check — they catch "the shell returned but the command didn't do its job",
# which an error-signature denylist keeps missing (see BUG-003/004/005/006).
def scenario_smoke(_args):
    return [
        ("uname", "EuroOS"), "free", "df", ("eurohealth", "100/100"),
        "ps", "ls /", ("echo serial-console-alive", "serial-console-alive"),
    ]


def scenario_users(args):
    # Three things have to be right or the add is rejected (each caught the hard way once):
    #   1. CAP_USER_ADMIN — the serial/desktop session is the unprivileged `euro` user, so
    #      each add must go via `sudo` (euro is in the sudoers); else EPERM.       [BUG-004]
    #   2. password policy — EuroID requires >=12 chars; else "password too short". [BUG-005]
    #   3. group must exist — built-ins are wheel/audit/net/vault/agent/users; `staff`
    #      is unknown.                                                              [BUG-006]
    # Positive assertion per add: the success line is "[euro/users] user '..' created (uid=".
    cmds = ["eurohealth", "free"]
    for i in range(1, args.users + 1):
        cmds.append((f"sudo eurousers add user{i:03d} LoadtestPw!{i:03d} users,net",
                     "created (uid="))
    cmds += [
        ("eurousers list", f"user{args.users:03d}"),          # last-created user is listed
        ("eurousers audit --verify-chain", "chain intact"),    # tamper-evident log holds
        "free", ("eurohealth", "100/100"),
    ]
    return cmds


def scenario_fs(_args):
    return [
        ("mkdir /lt", "created"), ("write /lt/a.txt hello-eurofs", "written"),
        ("cat /lt/a.txt", "hello-eurofs"), ("sha256sum /lt/a.txt", "/lt/a.txt"),
        ("ln -s /lt/a.txt /lt/link", "link"), ("readlink /lt/link", "/lt/a.txt"),
        ("cat /lt/link", "hello-eurofs"), ("cp /lt/a.txt /lt/b.txt", "b.txt"),
        ("ls /lt", "b.txt"), "df", ("scrub", "HEALTHY"),
    ]


# Special-character filenames (no space — the shell splits on it; no '/' — path separator;
# no '|' — pipeline). scon now buffers UTF-8, so Unicode names work end-to-end. Each is
# written then read back to prove the name round-trips byte-for-byte. NOTE: EuroFS caps
# names at 48 BYTES (DIRENT_NAME_CAP); Unicode chars are multi-byte, so names are kept short.
FSSTRESS_NAMES = [
    # ASCII specials
    "bang!", "hash#", "dollar$", "pct%", "caret^", "amp&", "star*", "qmark?",
    "paren()", "brack[]", "brace{}", "semi;", "colon:", "apos'", "tilde~",
    "comma,", "plus+", "eq=", "at@", "backtick`", "lt<gt>", "dash-x", "under_x",
    "dot.", "..dd", "...", "many.dots.in.it",
    # Unicode (Greek, Chinese, Japanese, Cyrillic, accented, emoji) — your request
    "Ελληνικά",          # Greek (16 bytes)
    "γειά-σου",          # Greek w/ hyphen
    "中文文件",            # Chinese (12 bytes)
    "日本語フォルダ",        # Japanese (18 bytes)
    "Привет",            # Cyrillic
    "café-naïve",        # Latin-1 accents
    "Köln-Straße",       # German eszett/umlaut
    "emoji😀🔥",          # emoji (4 bytes each)
    "naïve.café.txt",    # mixed accents + dots
    # length-limit probe (BYTE length vs the 48-byte cap)
    "L" * 48,            # exactly at the cap → OK
    "L" * 49,            # one over → must be rejected
    "L" * 200,           # well over → rejected
    "CaseX", "casex",    # case-sensitivity probe (same name if case-insensitive)
]


def scenario_fsstress(args):
    # Filesystem robustness battery: directory depth, many files in one dir, special-char
    # names (round-tripped), large files, and graceful errors on edge paths. Targets
    # `args.fsbase` (default /mnt).
    # IMPORTANT: /mnt is the SECOND virtio-blk disk (device 1), so it only mounts with
    # TWO disks attached — use `--disks 64M,2G` (NOT `--disks 2G`, which is device 0 only
    # and leaves /mnt unmounted → writes fall through to the 8 MiB root). The early
    # `fsdebug` line below prints the real fs size so this footgun is visible in the log.
    b = args.fsbase.rstrip("/")
    cmds = [("eurohealth", "100/100"), (f"fsdebug {b}", "blocks=")]  # log the real fs size
    # 0. scaffolding
    for d in (b, f"{b}/depth", f"{b}/many", f"{b}/weird", f"{b}/big"):
        cmds.append((f"mkdir {d}", "created"))
    # 1. directory DEPTH — incremental mkdir (no mkdir -p); write+read at the bottom.
    path = f"{b}/depth"
    for i in range(1, args.depth + 1):
        path = f"{path}/d{i:03d}"
        cmds.append((f"mkdir {path}", "created"))
    cmds.append((f"write {path}/deep.txt depth{args.depth}ok", "written"))
    cmds.append((f"cat {path}/deep.txt", f"depth{args.depth}ok"))  # path resolves at full depth
    # 2. MANY files in one directory (directory growth / multi-cluster dirs).
    for i in range(args.files):
        cmds.append((f"write {b}/many/f{i:04d} v{i}", "written"))
    cmds.append((f"ls {b}/many", f"f{args.files - 1:04d}"))  # last file is listed
    # 3. SPECIAL-CHAR names — write then read back; mismatch ⇒ name corruption.
    #    Names > 48 bytes (DIRENT_NAME_CAP) must now be REJECTED, not silently truncated
    #    (BUG-008 fix): assert the write errors instead of round-tripping.
    for i, nm in enumerate(FSSTRESS_NAMES):
        tag = f"w{i:02d}ok"
        if len(nm.encode("utf-8")) > 48:  # DIRENT_NAME_CAP is 48 BYTES, not chars
            cmds.append((f"write {b}/weird/{nm} {tag}", "InvalidPath"))  # rejected, not truncated
        else:
            cmds.append((f"write {b}/weird/{nm} {tag}", "written"))
            cmds.append((f"cat {b}/weird/{nm}", tag))
    # 4. LARGE files (non-sparse: truncate fills with zeros) — exercise big block chains.
    #    NOTE: needs a real /mnt (≥2 disks). On the 8 MiB root, ≥4 MiB fails as disk-full.
    for sz in (1 << 20, 4 << 20, 16 << 20, 64 << 20):  # 1, 4, 16, 64 MiB
        cmds.append((f"truncate -s {sz} {b}/big/f{sz}", "bytes"))
        cmds.append((f"stat {b}/big/f{sz}", f"Size: {sz}"))
    # 5. EDGE paths — must error GRACEFULLY (no hang/crash), not succeed.
    cmds += [
        (f"ls {b}/does-not-exist", "ls:"),        # error prefix only appears on the error path
        (f"cat {b}/depth", "cat:"),               # cat a directory → error
        (f"mkdir {b}/many", "AlreadyExists"),     # re-create an existing subdir → error
        (f"cat {b}/many/f0000", "v0"),            # sanity: a known file still reads
        ("scrub", "HEALTHY"), ("df", "EuroFS"), ("eurohealth", "100/100"),
    ]
    return cmds


def scenario_persist_write(_args):
    # Write a marker to the ROOT (/persist) — to be checked after a reboot on the same disk.
    return [
        ("mkdir /persist", "created"),
        ("write /persist/marker.txt SURVIVED-REBOOT", "written"),
        ("cat /persist/marker.txt", "SURVIVED-REBOOT"),
        ("scrub", "HEALTHY"),
    ]


def scenario_persist_check(_args):
    # After rebooting the SAME disk (--keep-disks): the marker must still be there ⇒ the
    # root is a real persistent on-disk EuroFS, not a wiped-on-boot RAM disk.
    return [
        ("cat /persist/marker.txt", "SURVIVED-REBOOT"),
        ("ls /persist", "marker.txt"),
        ("eurohealth", "100/100"),
    ]


SCENARIOS = {
    "smoke": scenario_smoke, "users": scenario_users, "fs": scenario_fs,
    "fsstress": scenario_fsstress,
    "persist-write": scenario_persist_write, "persist-check": scenario_persist_check,
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--image", default="eurokernel.img")
    ap.add_argument("--scenario", choices=SCENARIOS, default="smoke")
    ap.add_argument("--users", type=int, default=50)
    ap.add_argument("--fsbase", default="/mnt", help="fsstress target mountpoint")
    ap.add_argument("--depth", type=int, default=64, help="fsstress directory depth")
    ap.add_argument("--files", type=int, default=120, help="fsstress files-per-dir count")
    ap.add_argument("--disks", default="", help='comma sizes, e.g. "64M,2G"')
    ap.add_argument("--csv", default="loadtest-results.csv")
    ap.add_argument("--keep-disks", action="store_true",
                    help="reuse existing disk files (don't re-zero) — for reboot/persistence tests")
    ap.add_argument("--boot-timeout", type=int, default=400)
    a = ap.parse_args()

    ts = time.strftime("%Y%m%d-%H%M%S")
    serial_log = f"loadtest-serial-{ts}.log"
    sock_path = f"/tmp/euroos-scon-{ts}.sock"

    # Persistent scratch disks (NOT -snapshot — writes must survive).
    disks = []
    for j, spec in enumerate([s for s in a.disks.split(",") if s.strip()]):
        p = f"/tmp/loadtest-disk{j}-{spec}.raw"
        if a.keep_disks and os.path.exists(p):
            print(f"[loadtest] reusing existing disk {p} (--keep-disks)")
        else:
            with open(p, "wb") as f:
                f.truncate(parse_size(spec))
        disks.append(p)

    print(f"[loadtest] scenario={a.scenario} image={a.image} disks={disks}")
    print(f"[loadtest] serial log: {serial_log}  ·  CSV: {a.csv}")
    qemu = launch_qemu(a.image, disks, sock_path, serial_log)
    con = SerialConsole(sock_path, serial_log)
    rc = 0
    try:
        if not con.connect():
            print("[loadtest] FAILED: could not connect to serial socket"); return 2
        print("[loadtest] waiting for serial console …")
        con.boot(a.boot_timeout)
        print("[loadtest] serial console up — driving commands")

        cmds = SCENARIOS[a.scenario](a)
        with open(a.csv, "w", newline="") as cf:
            w = csv.writer(cf)
            w.writerow(["id", "command", "status", "elapsed_ms", "output_lines"])
            crashed = False
            errors = 0
            for i, item in enumerate(cmds, 1):
                cmd, expect = item if isinstance(item, tuple) else (item, None)
                try:
                    status, ms, lines = con.run(cmd)
                except CrashError as e:
                    status, ms, lines = "CRASH:" + str(e)[:60], 0, []
                # Positive baseline assertion: required output must be present.
                note = ""
                if status == "ok" and expect is not None \
                        and not any(expect in l for l in lines):
                    status = "err"
                    note = f"  (missing expected: {expect!r})"
                w.writerow([i, cmd, status, ms, len(lines)])
                cf.flush()
                tag = "✓" if status == "ok" else "✗"
                print(f"  [{i:>3}/{len(cmds)}] {tag} {status:<8} {ms:>6}ms  {cmd}{note}")
                if status == "err":
                    errors += 1
                if status.startswith("CRASH"):
                    crashed = True
                    print(f"  !! CRASH after {cmd!r} — see {serial_log}")
                    break
            rc = 1 if (crashed or errors) else 0
        if crashed:
            verdict = "CRASH/HANG detected"
        elif errors:
            verdict = f"{errors} command(s) FAILED (returned but did not succeed)"
        else:
            verdict = "completed clean"
        print(f"[loadtest] {verdict} · results → {a.csv}")
    finally:
        try:
            qemu.terminate(); qemu.wait(timeout=10)
        except Exception:
            qemu.kill()
        for d in disks:
            pass  # leave persistent disks in place for reuse / post-mortem
    return rc


if __name__ == "__main__":
    sys.exit(main())
