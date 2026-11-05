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
  python3 scripts/loadtest.py [--image eurokernel.img] [--scenario smoke|users|fs|...]
                              [--users N] [--disks "64M,2G"] [--csv results.csv]
  KVM / load scenarios (LOADTEST-R630, docs/LOADTEST-R630-2026-11.md):
  python3 scripts/loadtest.py --kvm --smp 4 --mem 16G --workdir DIR \
      --scenario procs|churn|fsio|net|soak [--disk-bus virtio|nvme|ahci]
      [--nic e1000e --bootopts net.driver=user --iommu --host-http P --hostfwd P --hammer K]
"""
import argparse, csv, json, os, re, socket, subprocess, sys, time

OVMF = next((c for c in ("/usr/share/ovmf/OVMF.fd", "/usr/share/OVMF/OVMF.fd",
                         "/usr/share/edk2-ovmf/x64/OVMF.fd") if os.path.exists(c)), None)
# Match ONLY real crash output, not EuroOS's benign boot self-tests (which deliberately
# print "breakpoint exception handled", "[isolation] … TERMINATED", "[j3-fault]", etc.).
# `[idt] PAGE FAULT` is a ring-0 page fault (the kernel halts); healthy boots only
# print `[idt] BREAKPOINT` (LOADTEST-R630: an NVMe boot under KVM faulted that way and
# the harness waited out the whole boot timeout instead of reporting it).
CRASH_RE = re.compile(r"KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT|\[idt\] PAGE FAULT")
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
        self._tail = ""
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
            # Scan across chunk borders: the panic path writes the UART byte by byte,
            # so "KERNEL PANIC" can arrive split over several recv() calls (LOADTEST-R630:
            # a real panic went unnoticed until the 900 s command timeout).
            window = self._tail + text
            self._tail = window[-256:]
            if CRASH_RE.search(window):
                # Let the kernel finish its dump (registers, backtrace, kmsg) before
                # the VM is killed: the only BUG-032 panic log ends at the CR3 line.
                self.drain(8)
                raise CrashError(window.strip().splitlines()[-1] if window.strip() else "crash")
            return text
        return ""

    def drain(self, seconds):
        """Copy whatever the guest prints for `seconds` into the log (no crash scan)."""
        end = time.time() + seconds
        while time.time() < end:
            try:
                data = self.sock.recv(65536)
            except (socket.timeout, OSError):
                continue
            if data:
                self.log.write(data)

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
        Returns (status, elapsed_ms, output_lines); `self.raw` keeps the whole serial
        text of the command (verdict lines printed outside the [scon] framing)."""
        self.buf = ""
        self.raw = ""
        self.sock.sendall((cmd + "\n").encode())
        t0 = time.time()
        try:
            self.wait_for(READY, timeout)
            status = "ok"
        except CrashError as e:
            self.raw = self.buf
            return ("CRASH:" + str(e)[:60], int((time.time() - t0) * 1000), [])
        self.raw = self.buf
        elapsed = int((time.time() - t0) * 1000)
        lines = [l[len("[scon] "):] for l in self.buf.splitlines()
                 if l.startswith("[scon] ") and not l.startswith("[scon] $ ")]
        # Intent check: the shell returned, but did the command actually succeed?
        if any(ERR_RE.search(l) for l in lines):
            status = "err"
        return (status, elapsed, lines)


def launch_qemu(image, disks, sock_path, serial_log, a=None):
    accel = ["-cpu", "qemu64,+smep,+smap"]
    if a is not None and a.kvm:
        accel = ["-enable-kvm", "-cpu", "host" + a.cpu_opts]
    mem = a.mem if a is not None else "1024"
    args = ["qemu-system-x86_64", "-machine", "q35", "-m", str(mem)] + accel
    if a is not None:
        args += ["-smp", str(a.smp)]
        if a.iommu:
            # Must precede the PCI devices it translates.
            args += ["-device", "intel-iommu,intremap=off"]
    args += ["-bios", OVMF,
             "-drive", f"format=raw,file={image}",          # NOT -snapshot (persistent)
             "-chardev", f"socket,id=ser0,path={sock_path},server=on,wait=off",
             "-serial", "chardev:ser0", "-display", "none", "-no-reboot"]
    if a is not None and a.nmi_on_hang:
        args += ["-monitor", f"unix:{sock_path}.mon,server=on,wait=off"]
    bus = a.disk_bus if a is not None else "virtio"
    for i, path in enumerate(disks):
        args += ["-drive", f"format=raw,file={path},if=none,id=ld{i}"]
        if bus == "nvme":
            args += ["-device", f"nvme,drive=ld{i},serial=lt{i}"]
        elif bus == "ahci":
            args += ["-device", f"ich9-ahci,id=ltahci{i}", "-device", f"ide-hd,drive=ld{i},bus=ltahci{i}.0"]
        else:
            args += ["-device", f"virtio-blk-pci,drive=ld{i},disable-modern=on"]
    if a is not None and a.nic:
        nd = "user,id=n0" + (",restrict=on" if a.restrict else "")
        if a.host_http:
            nd += a.host_http_peer.netdev()
        if a.hostfwd:
            nd += f",hostfwd=tcp:127.0.0.1:{a.hostfwd}-:80"
        args += ["-netdev", nd, "-device", f"{a.nic},netdev=n0"]
    if a is not None and a.pin:
        args = ["taskset", "-c", a.pin] + args
    return subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)


def nmi_probe(mon_path, con):
    """A hang: inject an NMI (the kernel's NMI handler prints where every core
    is, `[NMI PROBE]`), and keep 5 s of serial output in the log."""
    try:
        m = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        m.connect(mon_path)
        m.sendall(b"nmi\n")
        time.sleep(0.5)
        m.close()
        con.drain(5)
        print("  !! NMI injected; probe output in the serial log")
    except OSError as e:
        print(f"  !! NMI probe failed: {e}")


def prepare_image(a, ts):
    """With --workdir: boot from a private copy of the image (parallel VMs never
    share one), with the boot options (--bootopts, e.g. net.driver=user) on its ESP."""
    if not a.workdir:
        return a.image
    os.makedirs(a.workdir, exist_ok=True)
    img = os.path.join(a.workdir, "boot.img")
    if not (a.keep_disks and os.path.exists(img)):
        subprocess.run(["cp", "--sparse=always", a.image, img], check=True)
        if a.bootopts:
            conf = os.path.join(a.workdir, "euroos-boot.conf")
            with open(conf, "w") as f:
                f.write(a.bootopts.replace(";", "\n") + "\n")
            subprocess.run(["mcopy", "-o", "-i", img, conf, "::/EFI/BOOT/euroos-boot.conf"], check=True)
    return img


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


# ── LOADTEST-R630 (2026-11): KVM, disk buses, NIC, host-side peers, soak ──
# Scenario items may also be `C(...)` records: a command, an optional positive
# assertion on its [scon] output, an optional regex that must match the RAW serial
# stream of that command (for verdict lines the kernel prints outside the [scon]
# framing, e.g. `[churn-proc] ... OK`), and a per-command timeout.
class C:
    def __init__(self, cmd, expect=None, raw=None, timeout=60, raw_bad=None, baseline=None, max_ms=None):
        self.cmd, self.expect, self.raw, self.timeout, self.raw_bad = cmd, expect, raw, timeout, raw_bad
        self.max_ms = max_ms  # the command must answer within this time (perf regression)
        self.baseline = baseline  # ltstat field that must equal its first sampled value


# A spawned worker that is killed by the isolation handler did not do its job
# (BUG-016: every process whose arena lay above 1 GiB died on its first fetch).
ISOLATION_KILL = r"\[isolation\].*TERMINATED"


def scenario_procs(args):
    # Many processes at once: waves of signed musl programs started together as
    # preemptively scheduled background processes (own address space each), then
    # reaped by the desktop loop. After every wave the live-process count must be back
    # at its boot baseline (boot services keep running) and no worker may have been
    # killed by the isolation handler; frames/tasks are logged per sample.
    cmds = [C("ltstat", "ltstat ", baseline="bg_live")]
    for w in range(args.waves):
        cmds.append(C(f"spawn worker {args.procs}", "spawn: started", timeout=120))
        cmds.append(C("sleepwait 15", None))   # host-side pause: let them run + exit
        cmds.append(C("ltstat", "ltstat ", raw_bad=ISOLATION_KILL, baseline="bg_live"))
    # More processes in one go than the scheduler has free slots: the surplus must be
    # refused cleanly ("stopped early"), the rest must run and be reaped (BUG-015).
    cmds += [C(f"spawn worker {args.burst}", "spawn: started", timeout=300),
             C("sleepwait 20", None),
             C("ltstat", "ltstat ", raw_bad=ISOLATION_KILL, baseline="bg_live")]
    # Threads: musl pthread_create/join in many processes at once (BUG-017).
    cmds += [C(f"spawn mpthread {args.threads}", "spawn: started", timeout=120),
             C("sleepwait 20", None),
             C("ltstat", "ltstat ", raw_bad=ISOLATION_KILL, baseline="bg_live")]
    cmds += [C("ps", None), C("free", None), C("lockstat", None), C("eurohealth", "/100")]
    return cmds


def scenario_churn(args):
    s = args.churn_scale
    return [
        C("ltstat", "ltstat "),
        C(f"churn proc {16 * s}", "done", raw=r"\[churn-proc\] .*OK", timeout=60 * s),
        C(f"churn thread {s}", "done", raw=r"\[churn-thread\] .*OK", timeout=60 * s, raw_bad=r"\[churn-thread\] .*FAILED"),
        C(f"churn fork {max(1, s // 2)}", "done", raw=r"\[churn-fork\] .*OK", timeout=120 * s, raw_bad=r"\[churn-fork\] .*FAILED"),
        C(f"churn fs {s}", "done", raw=r"\[churn-fs\] .*OK", timeout=60 * s, raw_bad=r"\[churn-fs\] .*FAILED"),
        C(f"churn net {s}", "done", raw=r"\[churn-net\] .*OK", timeout=60 * s, raw_bad=r"\[churn-net\] .*FAILED"),
        C("ltstat", "ltstat "), C("lockstat", None), C("eurohealth", "/100"),
    ]


def scenario_fsio(args):
    # File I/O on the PERSISTENT root disk (virtio / NVMe / AHCI, chosen with
    # --disk-bus). Large files via truncate (zero-filled, really allocated), copies,
    # hashes, many small files, then a scrub. Run `persist-check-io` after a reboot
    # on the same disk (--keep-disks).
    b = args.iobase + "-io"
    cmds = [C("df", "EuroFS"), C(f"mkdir {b}", "created"), C(f"fsdebug {b}", "blocks="),
            C("echo /etc/hostname", "/etc/hostname")]  # a path argument is not stdin (BUG-022)
    for mib in args.io_sizes:
        sz = mib << 20
        # Long timeouts: the periodic scrub runs synchronously and can hold the
        # console for tens of seconds once hundreds of MiB are stored (BUG-023).
        cmds += [C(f"truncate -s {sz} {b}/z{mib}", "bytes", timeout=600),
                 # stat is metadata only: it must not read the file (BUG-022)
                 C(f"stat {b}/z{mib}", f"Size: {sz}", max_ms=3000, timeout=600),
                 C(f"cp {b}/z{mib} {b}/c{mib}", f"c{mib}", timeout=900),
                 C(f"sha256sum {b}/c{mib}", f"{b}/c{mib}", timeout=900)]
    # A file larger than the kernel heap: refused with an error or created, never a
    # panic (BUG-025); the system must still answer afterwards.
    cmds += [C(f"truncate -s {args.huge_mib << 20} {b}/huge", None, timeout=900),
             C(f"cp {b}/huge {b}/huge2", None, timeout=900),
             C(f"stat {b}/z{args.io_sizes[-1]}", "Size:", timeout=600),
             C(f"rm {b}/huge", None, timeout=600), C(f"rm {b}/huge2", None, timeout=600)]
    cmds += [C(f"mkdir {b}/small", "created")]
    for i in range(args.files):
        cmds.append(C(f"write {b}/small/f{i:05d} payload-{i}", "written", timeout=600))
    cmds += [C(f"ls {b}/small", f"f{args.files - 1:05d}", timeout=600),
             C(f"cat {b}/small/f00000", "payload-0", timeout=600),
             C(f"write {b}/marker SURVIVED-REBOOT-IO", "written", timeout=600),
             C("df", "EuroFS", timeout=600), C("scrub", "HEALTHY", timeout=900), C("ltstat", "ltstat ", timeout=600)]
    return cmds


def scenario_persist_check_io(args):
    b = args.iobase + "-io"
    cmds = [C(f"cat {b}/marker", "SURVIVED-REBOOT-IO", timeout=600), C(f"cat {b}/small/f00000", "payload-0", timeout=600)]
    for mib in args.io_sizes:
        cmds.append(C(f"stat {b}/c{mib}", f"Size: {mib << 20}", timeout=600))
    cmds += [C("scrub", "HEALTHY", timeout=900)]
    return cmds


def scenario_net(args):
    # Outbound: many sequential HTTP connections from the guest to a host server
    # (SLIRP guestfwd 10.0.2.100:80 -> 127.0.0.1:<--host-http>), and downloads of
    # the body (wget keeps at most 256 KiB per response). Inbound: the guest's
    # background httpd on :80, hammered from the host via hostfwd (--hammer).
    cmds = [C("net", None), C("httpd on", "listening")]
    for i in range(args.conns):
        cmds.append(C("fetch 10.0.2.100", "200 OK", timeout=60))
    for i in range(args.downloads):
        cmds.append(C(f"wget 10.0.2.101 {args.iobase}-dl{i % 4}.bin", f"saved: {args.http_size} bytes", timeout=120))
    cmds += [C("sleepwait 30", None), C("httpd status", "served"), C("netstat", None),
             C("ltstat", "ltstat "), C("lockstat", None), C("eurohealth", "/100")]
    return cmds


def scenario_soak(args):
    # Mixed load until --duration seconds have passed. Every round: a process wave,
    # FS write/read/hash/remove on the persistent root, outbound HTTP (small fetch,
    # a 200 KB download every 5th round), a churn round, then ltstat; lockstat and
    # free every 10th round. Inbound (httpd + --hammer) only if asked for.
    t_end = time.time() + args.duration
    d = args.iobase + "-soak"
    r = 0
    yield C(f"mkdir {args.iobase}", None)
    yield C(f"mkdir {d}", "created")
    if args.hammer:
        yield C("httpd on", "listening")
    while time.time() < t_end:
        yield C(f"spawn worker {args.procs}", "spawn: started", timeout=120)
        yield C(f"write {d}/f{r % 50} round-{r}", "written")
        yield C(f"cat {d}/f{r % 50}", f"round-{r}")
        yield C(f"truncate -s {8 << 20} {d}/big{r % 4}", "bytes", timeout=300)
        yield C(f"sha256sum {d}/big{r % 4}", f"{d}/big", timeout=300)
        yield C(f"rm {d}/big{(r + 2) % 4}", None)
        yield C("fetch 10.0.2.100", "200 OK", timeout=60)
        if r % 5 == 0:
            yield C(f"wget 10.0.2.101 {d}/dl.bin", f"saved: {args.http_size} bytes", timeout=120)
        if r % 3 == 0:
            yield C("churn proc 4", "done", raw=r"\[churn-proc\] .*OK", timeout=300)
        if r % 3 == 1:
            yield C("churn fs 1", "done", raw=r"\[churn-fs\] .*OK", timeout=300)
        if r % 3 == 2:
            yield C("churn net 1", "done", raw=r"\[churn-net\] .*OK", timeout=300)
        yield C("ltstat", "ltstat ", raw_bad=ISOLATION_KILL)
        if r % 10 == 0:
            yield C("lockstat", None, timeout=120)
            yield C("free", None)
        if args.heaptrace:
            if r + 1 == args.heaptrace:
                yield C("heapsnap", "heapsnap:")
            elif r + 1 > args.heaptrace and (r + 1 - args.heaptrace) % 50 == 0:
                yield C("heapdiff 25", "[heaptrace]", timeout=120)
        r += 1
    if args.heaptrace:
        yield C("heapdiff 60", "[heaptrace]", timeout=120)
    if args.hammer:
        yield C("httpd status", "served")
    yield C("scrub", "HEALTHY", timeout=900)
    yield C("lockstat", None, timeout=120)
    yield C("ltstat", "ltstat ")


def scenario_inbound(args):
    # BUG-019: inbound load (--hammer K host clients on the httpd through
    # --hostfwd) while the console must keep answering: one `echo` a second for
    # --duration seconds, each within --console-ms. Then the httpd and TCP
    # counters (netstat's first line).
    t_end = time.time() + args.duration
    yield C("httpd on", "listening")
    while time.time() < t_end:
        yield C("echo console-alive", "console-alive", timeout=120, max_ms=args.console_ms)
        yield C("sleepwait 1", None)
    yield C("httpd status", "served")
    yield C("netstat", "tcp: ")
    yield C("ltstat", "ltstat ")


def scenario_inbound_busy(args):
    # Net worker: the httpd while the shell is busy. --hammer K host clients on
    # the httpd (--hostfwd) for --duration seconds while the console runs only
    # long synchronous commands (churn fs, truncate + sha256sum of 8 MiB: 10-30 s
    # each under KVM), the ones that held the desktop loop and with it the
    # inbound connections. The harness's summary splits the clients' requests
    # into those that started during a command of >= 5 s and the rest. Needs a
    # disk root for --iobase (--disks 8G).
    t_end = time.time() + args.duration
    d = args.iobase + "-busy"
    yield C(f"mkdir {args.iobase}", None)
    yield C(f"mkdir {d}", "created")
    yield C("httpd on", "listening")
    r = 0
    while time.time() < t_end:
        yield C("churn fs 1", "done", raw=r"\[churn-fs\] .*OK", timeout=300)
        yield C(f"truncate -s {8 << 20} {d}/big{r % 2}", "bytes", timeout=300)
        yield C(f"sha256sum {d}/big{r % 2}", f"{d}/big", timeout=300)
        r += 1
    yield C("httpd status", "served")
    yield C("netstat", "tcp: ")
    yield C("ltstat", "ltstat ")


def scenario_cmds(args):
    # Ad-hoc: the shell commands given with --cmds, separated by ";" (no assertions).
    return [C(c.strip(), None, timeout=300) for c in args.cmds.split(";") if c.strip()]


SCENARIOS["cmds"] = scenario_cmds

SCENARIOS.update({"procs": scenario_procs, "churn": scenario_churn, "fsio": scenario_fsio,
                  "persist-check-io": scenario_persist_check_io, "net": scenario_net, "soak": scenario_soak,
                  "inbound": scenario_inbound, "inbound-busy": scenario_inbound_busy})


# ── host-side peers ──
class HostHttp:
    """Host-side HTTP peers for the guest, through SLIRP guestfwd with a command
    per connection (a guestfwd to a tcp: target is ONE persistent chardev
    connection, useless for many connections): 10.0.2.100:80 answers 1000 bytes,
    10.0.2.101:80 answers `size` bytes. Every answered request is one line in
    the hit log."""
    PEER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "loadtest-http-peer.py")

    def __init__(self, workdir, size):
        self.log = os.path.join(workdir or ".", "host-http-hits.log")
        open(self.log, "w").close()
        self.size = size

    def netdev(self):
        cmd = lambda n: f"cmd:python3 {self.PEER} {n} {self.log}"
        return (f",guestfwd=tcp:10.0.2.100:80-{cmd(1000)}"
                f",guestfwd=tcp:10.0.2.101:80-{cmd(self.size)}")

    @property
    def hits(self):
        try:
            return sum(1 for _ in open(self.log))
        except OSError:
            return 0

    def stop(self):
        pass


class Hammer:
    """K concurrent clients opening fresh TCP connections to the guest httpd
    (hostfwd 127.0.0.1:<port> -> guest :80); counts successes, errors, latency."""
    def __init__(self, port, workers):
        import threading
        self.port, self.stop_ev = port, threading.Event()
        self.ok = self.err = 0
        self.lat = []
        self.reqs = []  # (start, end, good) per request
        self.lock = threading.Lock()
        self.threads = [threading.Thread(target=self.loop, daemon=True) for _ in range(workers)]
        for t in self.threads:
            t.start()

    def loop(self):
        while not self.stop_ev.is_set():
            t0 = time.time()
            try:
                c = socket.create_connection(("127.0.0.1", self.port), timeout=10)
                c.sendall(b"GET / HTTP/1.0\r\nHost: euroos\r\n\r\n")
                c.settimeout(10)
                got = b""
                while True:
                    d = c.recv(4096)
                    if not d:
                        break
                    got += d
                c.close()
                good = got.startswith(b"HTTP/1.1 200") and b"EuroOS" in got
            except OSError:
                good = False
            t1 = time.time()
            with self.lock:
                self.reqs.append((t0, t1, good))
                if good:
                    self.ok += 1
                    self.lat.append(t1 - t0)
                else:
                    self.err += 1
            if not good:
                time.sleep(0.5)

    def stop(self, windows=()):
        """Totals; with `windows` [(t0, t1, cmd)] also the requests that started
        inside one of them ("busy") and the others ("idle")."""
        self.stop_ev.set()
        for t in self.threads:
            t.join(timeout=15)

        def stats(reqs):
            lat = sorted(e - s for s, e, g in reqs if g)
            pick = lambda q: round(lat[min(len(lat) - 1, int(q * len(lat)))] * 1000, 1) if lat else None
            return {"ok": len(lat), "err": sum(1 for r in reqs if not r[2]), "p50_ms": pick(0.5), "p99_ms": pick(0.99),
                    "max_ms": round(lat[-1] * 1000, 1) if lat else None}
        out = {"ok": self.ok, "err": self.err, **{k: v for k, v in stats(self.reqs).items() if k.endswith("_ms")}}
        if windows:
            inside = lambda s: any(w0 <= s < w1 for w0, w1, _ in windows)
            out["busy_windows"] = len(windows)
            out["busy_s"] = round(sum(w1 - w0 for w0, w1, _ in windows))
            out["busy"] = stats([r for r in self.reqs if inside(r[0])])
            out["idle"] = stats([r for r in self.reqs if not inside(r[0])])
        return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--image", default="eurokernel.img")
    ap.add_argument("--scenario", choices=SCENARIOS, default="smoke")
    ap.add_argument("--users", type=int, default=50)
    ap.add_argument("--fsbase", default="/mnt", help="fsstress target mountpoint")
    ap.add_argument("--depth", type=int, default=64, help="fsstress directory depth")
    ap.add_argument("--files", type=int, default=120, help="fsstress/fsio files-per-dir count")
    ap.add_argument("--disks", default="", help='comma sizes, e.g. "64M,2G"')
    ap.add_argument("--csv", default="loadtest-results.csv")
    ap.add_argument("--keep-disks", action="store_true",
                    help="reuse existing disk files (don't re-zero) — for reboot/persistence tests")
    ap.add_argument("--boot-timeout", type=int, default=400)
    # LOADTEST-R630 additions
    ap.add_argument("--kvm", action="store_true", help="-enable-kvm -cpu host (default: TCG qemu64)")
    ap.add_argument("--smp", type=int, default=1)
    ap.add_argument("--cpu-opts", default="", help='appended to -cpu host, e.g. ",host-phys-bits=off,phys-bits=39" (BUG-021)')
    ap.add_argument("--mem", default="1024", help="guest RAM, QEMU syntax (e.g. 16G)")
    ap.add_argument("--pin", default="", help="taskset CPU list for the whole QEMU process")
    ap.add_argument("--disk-bus", choices=["virtio", "nvme", "ahci"], default="virtio")
    ap.add_argument("--workdir", default="", help="private dir: image copy, disks, logs")
    ap.add_argument("--bootopts", default="", help='ESP boot options, e.g. "net.driver=user"')
    ap.add_argument("--iommu", action="store_true", help="-device intel-iommu,intremap=off")
    ap.add_argument("--nic", default="", help="NIC model, e.g. e1000e (SLIRP user network)")
    ap.add_argument("--nmi-on-hang", action="store_true", help="QEMU monitor socket; on a HANG inject an NMI so the kernel prints its probe before the VM is killed")
    ap.add_argument("--restrict", action="store_true", help="SLIRP restrict=on: no route out of the host (the update check cannot reach euro-os.eu); guestfwd/hostfwd still work")
    ap.add_argument("--host-http", action="store_true", help="host HTTP peers: guest 10.0.2.100 (1000 B), 10.0.2.101 (--http-size B)")
    ap.add_argument("--http-size", type=int, default=200 * 1024)
    ap.add_argument("--hostfwd", type=int, default=0, help="host port forwarded to guest :80")
    ap.add_argument("--hammer", type=int, default=0, help="concurrent host clients on --hostfwd")
    ap.add_argument("--procs", type=int, default=64, help="processes per spawn wave")
    ap.add_argument("--waves", type=int, default=5)
    ap.add_argument("--burst", type=int, default=300, help="procs: processes in one spawn (> free slots)")
    ap.add_argument("--threads", type=int, default=16, help="procs: mpthread processes at once")
    ap.add_argument("--churn-scale", type=int, default=4)
    ap.add_argument("--io-sizes", default="16,64,256", help="fsio file sizes in MiB")
    ap.add_argument("--huge-mib", type=int, default=1024, help="fsio: a file larger than the kernel heap")
    ap.add_argument("--conns", type=int, default=200, help="net: sequential outbound connections")
    ap.add_argument("--console-ms", type=int, default=2000, help="inbound: limit for one console echo under load")
    ap.add_argument("--downloads", type=int, default=50)
    ap.add_argument("--duration", type=int, default=4 * 3600, help="soak seconds")
    ap.add_argument("--heaptrace", type=int, default=0, metavar="ROUNDS",
                    help="soak on a heaptrace image (BUG-027): heapsnap after ROUNDS warm-up rounds, heapdiff every 50 rounds and at the end")
    ap.add_argument("--iobase", default="/home/euro/lt", help="fsio/soak directory (the euro user can write here on a disk root)")
    ap.add_argument("--stop-on-err", action="store_true")
    ap.add_argument("--cmds", default="", help="scenario cmds: shell commands separated by \";\"")
    a = ap.parse_args()
    a.io_sizes = [int(x) for x in a.io_sizes.split(",") if x.strip()]

    ts = time.strftime("%Y%m%d-%H%M%S")
    wd = a.workdir or "."
    if a.workdir:
        os.makedirs(a.workdir, exist_ok=True)
        if not os.path.isabs(a.csv):
            a.csv = os.path.join(a.workdir, a.csv)
    serial_log = os.path.join(wd, f"loadtest-serial-{ts}.log")
    sock_path = f"/tmp/euroos-scon-{ts}-{os.getpid()}.sock"
    image = prepare_image(a, ts)

    # Persistent scratch disks (NOT -snapshot — writes must survive).
    disks = []
    for j, spec in enumerate([s for s in a.disks.split(",") if s.strip()]):
        p = os.path.join(a.workdir, f"disk{j}-{spec}.raw") if a.workdir else f"/tmp/loadtest-disk{j}-{spec}.raw"
        if a.keep_disks and os.path.exists(p):
            print(f"[loadtest] reusing existing disk {p} (--keep-disks)")
        else:
            with open(p, "wb") as f:
                f.truncate(parse_size(spec))
        disks.append(p)

    print(f"[loadtest] scenario={a.scenario} image={image} disks={disks} bus={a.disk_bus} "
          f"kvm={a.kvm} smp={a.smp} mem={a.mem} nic={a.nic or '-'} bootopts={a.bootopts or '-'} iommu={a.iommu}")
    print(f"[loadtest] serial log: {serial_log}  ·  CSV: {a.csv}")
    host_http = HostHttp(a.workdir, a.http_size) if a.host_http else None
    a.host_http_peer = host_http
    qemu = launch_qemu(image, disks, sock_path, serial_log, a)
    con = SerialConsole(sock_path, serial_log)
    rc = 0
    hammer = None
    busy_windows = []
    t_boot = time.time()
    samples_path = a.csv[:-4] + "-ltstat.csv" if a.csv.endswith(".csv") else a.csv + "-ltstat.csv"
    try:
        if not con.connect():
            print("[loadtest] FAILED: could not connect to serial socket"); return 2
        print("[loadtest] waiting for serial console …")
        try:
            con.boot(a.boot_timeout)
        except CrashError as e:
            # A boot that hangs before the console is up (BUG-031/033: silence after
            # a glibc self-test) gets the NMI probe too, not only a hung command.
            if a.nmi_on_hang and ("timeout" in str(e) or "silent" in str(e)):
                nmi_probe(sock_path + ".mon", con)
            raise
        print(f"[loadtest] serial console up after {time.time() - t_boot:.0f}s — driving commands", flush=True)
        if a.hammer and a.hostfwd:
            hammer = Hammer(a.hostfwd, a.hammer)

        cmds = SCENARIOS[a.scenario](a)
        total = len(cmds) if isinstance(cmds, list) else "?"
        quiet = not isinstance(cmds, list)   # the soak: print errors + every 25th row
        t_start = time.time()
        busy_windows = []  # console commands of >= 5 s: (t0, t1, cmd)
        with open(a.csv, "w", newline="") as cf, open(samples_path, "w", newline="") as sf:
            w = csv.writer(cf)
            w.writerow(["id", "command", "status", "elapsed_ms", "output_lines", "t_s"])
            sw = None
            baselines = {}
            crashed = False
            errors = 0
            for i, item in enumerate(cmds, 1):
                if not isinstance(item, C):
                    cmd, expect = item if isinstance(item, tuple) else (item, None)
                    item = C(cmd, expect)
                cmd, expect = item.cmd, item.expect
                if cmd.startswith("sleepwait "):         # host-side pause, not a guest command
                    time.sleep(int(cmd.split()[1]))
                    continue
                t_cmd = time.time()
                try:
                    status, ms, lines = con.run(cmd, timeout=item.timeout)
                except CrashError as e:
                    status, ms, lines = "CRASH:" + str(e)[:60], 0, []
                if time.time() - t_cmd >= 5:
                    busy_windows.append((t_cmd, time.time(), cmd))
                # Positive baseline assertion: required output must be present.
                note = ""
                if status == "ok" and expect is not None \
                        and not any(expect in l for l in lines):
                    status = "err"
                    note = f"  (missing expected: {expect!r})"
                if status == "ok" and item.raw and not re.search(item.raw, con.raw):
                    status = "err"
                    note = f"  (missing verdict /{item.raw}/)"
                if status == "ok" and item.max_ms is not None and ms > item.max_ms:
                    status = "err"
                    note = f"  (took {ms} ms, limit {item.max_ms} ms)"
                if status == "ok" and item.raw_bad and re.search(item.raw_bad, con.raw):
                    status = "err"
                    note = f"  (verdict /{item.raw_bad}/)"
                tnow = round(time.time() - t_start, 1)
                for l in lines:
                    if l.startswith("ltstat "):
                        kv = dict(x.split("=", 1) for x in l.split()[1:] if "=" in x)
                        if item.baseline and item.baseline in kv:
                            base = baselines.setdefault(item.baseline, kv[item.baseline])
                            if status == "ok" and kv[item.baseline] != base:
                                status = "err"
                                note = f"  ({item.baseline}={kv[item.baseline]}, baseline {base})"
                        if sw is None:
                            sw = csv.DictWriter(sf, fieldnames=["t_s", "cmd_id"] + list(kv))
                            sw.writeheader()
                        sw.writerow({"t_s": tnow, "cmd_id": i, **kv})
                        sf.flush()
                w.writerow([i, cmd, status, ms, len(lines), tnow])
                cf.flush()
                tag = "✓" if status == "ok" else "✗"
                if status != "ok" or not quiet or i % 25 == 0:
                    print(f"  [{i:>5}/{total}] {tag} {status:<8} {ms:>7}ms t={tnow:>7}s  {cmd}{note}", flush=True)
                if status == "err":
                    errors += 1
                    if a.stop_on_err:
                        break
                if status.startswith("CRASH"):
                    crashed = True
                    if a.nmi_on_hang and ("timeout" in status or "silent" in status):
                        nmi_probe(sock_path + ".mon", con)
                    print(f"  !! CRASH after {cmd!r} — see {serial_log}")
                    break
            rc = 1 if (crashed or errors) else 0
        if crashed:
            verdict = "CRASH/HANG detected"
        elif errors:
            verdict = f"{errors} command(s) FAILED (returned but did not succeed)"
        else:
            verdict = "completed clean"
        print(f"[loadtest] {verdict} · results → {a.csv} · ltstat samples → {samples_path}")
    finally:
        if hammer:
            print(f"[loadtest] hammer (inbound httpd, {a.hammer} clients): {json.dumps(hammer.stop(busy_windows))}")
        if host_http:
            print(f"[loadtest] host HTTP server served {host_http.hits} request(s)")
            host_http.stop()
        try:
            qemu.terminate(); qemu.wait(timeout=10)
        except Exception:
            qemu.kill()
        try:
            os.unlink(sock_path)
        except OSError:
            pass
    return rc


if __name__ == "__main__":
    sys.exit(main())
