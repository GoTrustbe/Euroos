#!/usr/bin/env python3
"""GUI smoke test and monkey soak for the EuroOS desktop.

The load tests drive the shell over the serial console; nothing drove the desktop.
This boots an image in QEMU and works it like a user would, with a real pointer and
keyboard injected over QMP (USB tablet + keyboard on xHCI, or virtio-input):

  smoke (default)   every dock app is opened, used a little (clicks, typing, keys),
                    its window is moved and closed; then Alt-Tab, the workspaces,
                    the launcher search and the context menu. One screenshot per app.
  --soak MINUTES    after the smoke pass: random clicks, drags, keys, text, dock and
                    window-button presses for that long. `--seed` makes the action
                    sequence reproducible; every action is written to actions.log.

After every step the guest must still be alive:
  * no crash signature on the serial console (KERNEL PANIC, ring-0 faults, ...);
  * QEMU still runs the guest;
  * the desktop loop answers: an `echo` sent to the serial shell console (`scon`,
    polled once per desktop-loop iteration) comes back within the timeout;
  * where a step must change the screen (a window opens, moves, closes), two
    screendumps differ, and the window is found by its three title-bar buttons.
A terminal command typed with the injected keyboard must also appear on the
serial console (`[e2e] $ ...`): that proves key -> scancode -> shell -> output.

Everything lives in a private work directory (image copy, QMP and serial sockets,
logs, screenshots): the shared /tmp/ek-qmp.sock of the older scripts is not used,
and the script only ever stops the QEMU process it started itself.

  python3 scripts/gui-smoke.py --image eurokernel.img --workdir /tmp/gui-smoke-1
  python3 scripts/gui-smoke.py --image eurokernel.img --kvm --smp 4 --mem 4G \\
      --soak 30 --seed 20261110 --workdir /srv/tmp/gui-soak

Exit status: 0 = every step passed, 1 = a step failed, 2 = crash or hang. A step
that fails on a tracked, open bug (KNOWN_ISSUES below) is reported as "known" and
does not count. Details: docs/GUI-TESTING.md.
"""
import argparse, codecs, json, os, random, re, shlex, socket, struct, subprocess, sys, tempfile, threading, time, zlib

OVMF = next((c for c in ("/usr/share/ovmf/OVMF.fd", "/usr/share/OVMF/OVMF.fd",
                         "/usr/share/edk2-ovmf/x64/OVMF.fd", "/usr/share/qemu/OVMF.fd") if os.path.exists(c)), None)

# Real crash output only. The boot self-tests of a selftest image print "[idt] BREAKPOINT",
# "[isolation] ... TERMINATED" and the word "watchdog" on purpose (see loadtest.py, BUG-001).
CRASH_RE = re.compile(
    r"KERNEL PANIC|\[PANIC\]|panicked at|DOUBLE FAULT|GENERAL PROTECTION FAULT|\[idt\] PAGE FAULT"
    r"|\[idt\] INVALID OPCODE|WATCHDOG|\[timer-probe\] FROZEN|memory allocation of \d+ bytes failed")

# compositor.rs: the dock (DOCK_X 14, DOCK_W 62, TILE_TOP 78, TILE 42, TILE_GAP 8).
DOCK_APPS = ["files", "notes", "clock", "browser", "terminal", "settings", "store", "star", "text", "monitor", "log"]
DOCK_CX = 14 + 31
def dock_tile(i):
    return (DOCK_CX, 78 + i * 50 + 21)
BRAND = (DOCK_CX, 14 + 27)          # the EU mark: opens the launcher
SIDEBAR_W, PANEL_W, TITLEBAR_H = 90, 284, 44
# The three title-bar buttons (close, minimize, maximize): 13 px, at x+14/34/54, y+15.
DOT_RED, DOT_YEL, DOT_GRN = bytes((0xEC, 0x6A, 0x5E)), bytes((0xF4, 0xBF, 0x50)), bytes((0x61, 0xC5, 0x54))

# Character -> (qcode, shift). US QWERTY is what the public image boots with; selftest
# images switch to be-azerty (the table of scripts/qmp-input.py).
US = {c: (c, False) for c in "abcdefghijklmnopqrstuvwxyz0123456789"}
US.update({c.upper(): (c, True) for c in "abcdefghijklmnopqrstuvwxyz"})
US.update({" ": ("spc", False), "-": ("minus", False), "_": ("minus", True), "=": ("equal", False),
           "+": ("equal", True), ".": ("dot", False), ",": ("comma", False), "/": ("slash", False),
           "?": ("slash", True), ";": ("semicolon", False), ":": ("semicolon", True),
           "!": ("1", True), "(": ("9", True), ")": ("0", True), "*": ("8", True), "&": ("7", True)})
AZERTY = {}
for _c in "bcdefghijklnoprstuvxy":
    AZERTY[_c] = (_c, False)
    AZERTY[_c.upper()] = (_c, True)
AZERTY.update({"a": ("q", False), "A": ("q", True), "q": ("a", False), "Q": ("a", True),
               "z": ("w", False), "Z": ("w", True), "w": ("z", False), "W": ("z", True),
               "m": ("semicolon", False), "M": ("semicolon", True)})
for _c in "1234567890":
    AZERTY[_c] = (_c, True)
AZERTY.update({" ": ("spc", False), ":": ("dot", False), "/": ("dot", True), ".": ("comma", True),
               ";": ("comma", False), ",": ("m", False), "?": ("m", True), "-": ("equal", False),
               "_": ("equal", True), "=": ("slash", False), "+": ("slash", True),
               "&": ("1", False), "!": ("8", False), "(": ("5", False), ")": ("minus", False)})
# Steps that fail because of a tracked, open bug (docs/LOADTEST-TRACKER.md). They are
# reported as "known" and do not fail the run; remove the entry when the bug is fixed.
KNOWN_ISSUES = {
}
# AltGr (BUG-052): per layout, the keys that give  @ # { } [ ] ~ euro \ |  in that order.
# A string is a qcode pressed with AltGr (alt_r) held; ("plain", qcode) is pressed alone
# (German QWERTZ has # on a key of its own).
ALTGR_WANT = "@#{}[]~\u20ac\\|"
ALTGR_KEYS = {
    "be-azerty": ["2", "3", "9", "0", "bracket_left", "bracket_right", "slash", "e", "less", "1"],
    "fr-azerty": ["0", "3", "4", "equal", "5", "minus", "2", "e", "8", "6"],
    "de-qwertz": ["q", ("plain", "backslash"), "7", "0", "8", "9", "bracket_right", "e", "minus", "less"],
}
SPECIAL_KEYS = ["ret", "esc", "tab", "backspace", "delete", "up", "down", "left", "right",
                "home", "end", "pgup", "pgdn", "f1", "f2", "f5", "f11", "insert", "spc"]


class Fatal(Exception):
    """The guest crashed, hung or went away: no point in further steps."""


class Serial:
    """COM1 over a unix socket: a reader thread keeps the console drained (an unread
    console stalls the guest in UART writes, BUG-036), scans for crashes and keeps
    the text for marker searches."""

    def __init__(self, path):
        self.text = ""
        self.crash = None
        self.lock = threading.Lock()
        self.closed = False
        for _ in range(200):
            try:
                self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.sock.connect(path)
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise Fatal("serial socket did not appear")
        self.sock.settimeout(0.5)
        threading.Thread(target=self._reader, daemon=True).start()

    def _reader(self):
        tail = ""
        # Incremental: the UART hands over a character of several bytes (the euro sign
        # of the AltGr step) in pieces, and decoding each piece alone made three
        # replacement characters of it.
        decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        while not self.closed:
            try:
                data = self.sock.recv(65536)
            except socket.timeout:
                continue
            except OSError:
                break
            if not data:
                break
            chunk = decoder.decode(data)
            with self.lock:
                self.text += chunk
                if len(self.text) > 64 << 20:            # a soak must not eat the host
                    self.text = self.text[-(32 << 20):]
            window = tail + chunk
            tail = window[-256:]
            m = CRASH_RE.search(window)
            if m and not self.crash:
                line = window[window.rfind("\n", 0, m.start()) + 1:].split("\n")[0]
                self.crash = line.strip() or m.group(0)

    def mark(self):
        with self.lock:
            return len(self.text)

    def since(self, pos):
        with self.lock:
            return self.text[pos:]

    def send_line(self, line):
        self.sock.sendall((line + "\n").encode())

    def wait_for(self, needle, timeout, pos=0):
        end = time.time() + timeout
        while time.time() < end:
            if needle in self.since(pos):
                return True
            if self.crash:
                return False
            time.sleep(0.05)
        return needle in self.since(pos)


class Qmp:
    def __init__(self, path):
        for _ in range(200):
            try:
                self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.sock.connect(path)
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise Fatal("QMP socket did not appear")
        self.f = self.sock.makefile("rwb", buffering=0)
        self.f.readline()
        self.lock = threading.Lock()
        self.cmd("qmp_capabilities")

    def cmd(self, execute, **arguments):
        msg = {"execute": execute}
        if arguments:
            msg["arguments"] = arguments
        with self.lock:
            try:
                self.f.write((json.dumps(msg) + "\n").encode())
                while True:                      # skip asynchronous events
                    line = self.f.readline()
                    if not line:
                        raise Fatal("QMP connection closed (QEMU gone)")
                    reply = json.loads(line.decode())
                    if "return" in reply or "error" in reply:
                        return reply
            except OSError as e:
                raise Fatal(f"QMP: {e}")


class Screen:
    """A screendump (binary PPM) with the few image operations the test needs."""

    def __init__(self, path):
        raw = open(path, "rb").read()
        parts = raw.split(None, 4)
        if len(parts) < 5 or parts[0] != b"P6":
            raise Fatal(f"screendump {path} is not a P6 PPM")
        self.w, self.h = int(parts[1]), int(parts[2])
        self.rgb = parts[4][:self.w * self.h * 3]
        if len(self.rgb) != self.w * self.h * 3:
            raise Fatal(f"screendump {path} is truncated")

    def differs(self, other, min_pixels=24, region=None):
        """More than `min_pixels` different pixels (inside region = x0,y0,x1,y1)?"""
        if (self.w, self.h) != (other.w, other.h):
            return True
        if self.rgb == other.rgb:
            return False
        x0, y0, x1, y1 = region or (0, 0, self.w, self.h)
        n = 0
        for y in range(max(y0, 0), min(y1, self.h)):
            a = self.rgb[(y * self.w + x0) * 3:(y * self.w + x1) * 3]
            b = other.rgb[(y * self.w + x0) * 3:(y * self.w + x1) * 3]
            if a != b:
                n += sum(1 for i in range(0, len(a), 3) if a[i:i + 3] != b[i:i + 3])
                if n > min_pixels:
                    return True
        return False

    def px(self, x, y):
        o = (y * self.w + x) * 3
        return self.rgb[o:o + 3]

    def windows(self):
        """Origins (x, y) of the windows whose title bar is visible: the red, yellow
        and green button 20 px apart on one row. Topmost-first is not known."""
        found, start = [], 0
        while True:
            i = self.rgb.find(DOT_RED, start)
            if i < 0:
                break
            start = i + 3
            if i % 3:
                continue
            p = i // 3
            x, y = p % self.w, p // self.w
            if x < 5 or y < 5 or x + 45 >= self.w or y + 5 >= self.h:
                continue
            # (x, y) is the centre of the red button when the button's colour is
            # 5 px away in all four directions and the other two buttons follow.
            if (self.px(x - 5, y) == DOT_RED and self.px(x + 5, y) == DOT_RED
                    and self.px(x, y - 5) == DOT_RED and self.px(x, y + 5) == DOT_RED
                    and self.px(x + 20, y) == DOT_YEL and self.px(x + 40, y) == DOT_GRN):
                wx, wy = x - 20, y - 21          # button centre = window origin + (20, 21)
                if not any(abs(wx - fx) < 8 and abs(wy - fy) < 8 for fx, fy in found):
                    found.append((wx, wy))
        return found

    def save_png(self, path):
        raw = b"".join(b"\x00" + self.rgb[y * self.w * 3:(y + 1) * self.w * 3] for y in range(self.h))
        def chunk(tag, data):
            return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))
        with open(path, "wb") as f:
            f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", self.w, self.h, 8, 2, 0, 0, 0))
                    + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))


class Guest:
    def __init__(self, a):
        self.a = a
        self.wd = a.workdir
        self.slow = a.slow
        self.results = []          # (name, ok, detail)
        self.known = []            # steps that failed on a known, open bug
        self.echo_n = 0
        self.max_echo_ms = 0
        self.shot_n = 0
        self.actions = open(os.path.join(self.wd, "actions.log"), "w", buffering=1)
        self.w, self.h = 1280, 800
        self.layout = US

    # ── boot ────────────────────────────────────────────────────────────────
    def boot(self):
        a = self.a
        img = os.path.join(self.wd, "boot.img")
        subprocess.run(["cp", "--sparse=always", a.image, img], check=True)
        self.qmp_path = os.path.join(self.wd, "qmp.sock")
        self.ser_path = os.path.join(self.wd, "serial.sock")
        self.serial_log = os.path.join(self.wd, "serial.log")
        accel = ["-enable-kvm", "-cpu", "host" + a.cpu_opts] if a.kvm else ["-cpu", "qemu64,+smep,+smap"]
        args = ["qemu-system-x86_64", "-machine", "q35", "-m", a.mem, "-smp", str(a.smp)] + accel + [
            "-bios", OVMF, "-drive", f"format=raw,file={img}",
            "-chardev", f"socket,id=ser0,path={self.ser_path},server=on,wait=off,logfile={self.serial_log},logappend=off",
            "-serial", "chardev:ser0", "-display", "none", "-no-reboot",
            "-qmp", f"unix:{self.qmp_path},server=on,wait=off"]
        if a.input == "virtio":
            args += ["-device", "virtio-keyboard-pci", "-device", "virtio-tablet-pci"]
        else:
            args += ["-device", "qemu-xhci,id=xhci", "-device", "usb-kbd,bus=xhci.0", "-device", "usb-tablet,bus=xhci.0"]
        if a.net == "none":
            args += ["-nic", "none"]
        else:
            # restrict=on: the guest sees a network (DHCP, the SLIRP DNS) but nothing
            # it sends leaves the host.
            args += ["-netdev", "user,id=n0,restrict=on", "-device", "virtio-net-pci,netdev=n0"]
        args += shlex.split(a.extra)
        if a.pin:
            args = ["taskset", "-c", a.pin] + args
        with open(os.path.join(self.wd, "qemu.cmdline"), "w") as f:
            f.write(" ".join(args) + "\n")
        self.qemu = subprocess.Popen(args, stdout=subprocess.DEVNULL,
                                     stderr=open(os.path.join(self.wd, "qemu.stderr"), "wb"))
        with open(os.path.join(self.wd, "qemu.pid"), "w") as f:
            f.write(str(self.qemu.pid) + "\n")
        self.serial = Serial(self.ser_path)
        self.qmp = Qmp(self.qmp_path)
        t0 = time.time()
        print(f"[gui-smoke] QEMU pid {self.qemu.pid}, booting ({'KVM' if a.kvm else 'TCG'}, {a.smp} cpu, {a.mem})...", flush=True)
        if not self.serial.wait_for("[desktop] interactive loop started", a.boot_timeout):
            self.fatal("boot", "no desktop within the boot timeout" if not self.serial.crash else self.serial.crash)
        if not self.serial.wait_for("[scon-ready]", 120 * self.slow):
            self.fatal("boot", "serial console (scon) not ready")
        boot_s = int(time.time() - t0)
        log = self.serial.since(0)
        if "keyboard layout: be-azerty" in log and a.layout == "auto" or a.layout == "be-azerty":
            self.layout = AZERTY
        time.sleep(3 * self.slow)       # the first full render
        s = self.shot()
        self.w, self.h = s.w, s.h
        self.record("boot", True, f"desktop after {boot_s} s, screen {self.w}x{self.h}, layout {'be-azerty' if self.layout is AZERTY else 'us'}")
        # The release image opens the first-run tour on the first desktop (not the
        # self-test image, not with ui.tour=off). Clicks and typed keys pass through
        # it, but the card would be in every comparison: close it with Esc, the key
        # it keeps for itself, and count that as a step.
        if "[tour] first-run tour shown" in self.serial.since(0):
            s.save_png(os.path.join(self.wd, "tour-step-1.png"))
            pos = self.serial.mark()
            self.key("esc")
            closed = self.serial.wait_for("[tour] closed", 20 * self.slow, pos)
            self.nap(1.0)
            after = self.shot("tour-closed")
            self.record("first-run tour: shown on the first desktop, Esc closes it",
                        closed and after.differs(s, min_pixels=2000), "" if closed else "no [tour] closed line on the serial log")
            s = after
        return s

    # ── results ─────────────────────────────────────────────────────────────
    def record(self, name, ok, detail=""):
        tag = "ok  " if ok else "FAIL"
        if not ok and name in KNOWN_ISSUES:
            tag, ok = "known", True
            detail = (KNOWN_ISSUES[name] + (", " + detail if detail else "")) + " (open; not counted)"
            self.known.append(name)
        self.results.append((name, ok, detail))
        print(f"[gui-smoke] {tag} {name}{': ' + detail if detail else ''}", flush=True)

    def fatal(self, name, detail):
        self.record(name, False, detail)
        # Whatever the kernel still has to say (panic dump, NMI probe) goes in the log.
        try:
            self.qmp.cmd("inject-nmi")
        except Exception:
            pass
        time.sleep(6)
        try:
            self.shot("fatal")
        except Exception:
            pass
        raise Fatal(detail)

    # ── liveness ────────────────────────────────────────────────────────────
    def alive(self, name):
        """The guest did not crash and its desktop loop still turns."""
        if self.serial.crash:
            self.fatal(name, "CRASH on serial: " + self.serial.crash)
        if self.qemu.poll() is not None:
            tail = self.serial.since(max(0, self.serial.mark() - 400))
            what = "guest powered off" if "S5 shutdown" in tail else f"QEMU exited (status {self.qemu.returncode})"
            self.fatal(name, what)
        self.echo_n += 1
        token = f"gs-alive-{self.echo_n}"
        pos = self.serial.mark()
        t0 = time.time()
        self.serial.send_line("echo " + token)
        ok = self.serial.wait_for("[scon] " + token, self.a.alive_timeout, pos)
        ms = int((time.time() - t0) * 1000)
        self.max_echo_ms = max(self.max_echo_ms, ms)
        if self.serial.crash:
            self.fatal(name, "CRASH on serial: " + self.serial.crash)
        if not ok:
            self.fatal(name, f"HANG: the desktop loop did not answer the serial console within {self.a.alive_timeout} s")
        if ms > 5000 * self.slow:
            self.record(name + " (stall)", True, f"desktop loop answered after {ms} ms")
        return ms

    # ── input ───────────────────────────────────────────────────────────────
    def _send(self, events):
        r = self.qmp.cmd("input-send-event", events=events)
        if "error" in r:
            raise Fatal(f"QMP input-send-event: {r['error']}")

    def nap(self, s):
        time.sleep(s * self.slow)

    def move(self, x, y):
        x, y = max(0, min(self.w - 1, int(x))), max(0, min(self.h - 1, int(y)))
        self._send([{"type": "abs", "data": {"axis": "x", "value": x * 32767 // max(self.w - 1, 1)}},
                    {"type": "abs", "data": {"axis": "y", "value": y * 32767 // max(self.h - 1, 1)}}])

    def button(self, down, button="left"):
        self._send([{"type": "btn", "data": {"down": down, "button": button}}])

    def click(self, x, y, button="left", double=False):
        self.actions.write(f"{'dclick' if double else 'click'} {button} {int(x)} {int(y)}\n")
        self.move(x, y)
        self.nap(0.12)
        for _ in range(2 if double else 1):
            self.button(True, button)
            self.nap(0.12)
            self.button(False, button)
            self.nap(0.12)
        self.nap(0.25)

    def wheel(self, notches):
        """Turn the mouse wheel: positive = towards the user (scroll down)."""
        self.actions.write(f"wheel {notches}\n")
        b = "wheel-down" if notches > 0 else "wheel-up"
        for _ in range(abs(notches)):
            self.button(True, b)
            self.button(False, b)
            self.nap(0.06)

    def scon(self, line):
        """Run a shell command over the serial console; returns the serial position before it."""
        pos = self.serial.mark()
        self.serial.send_line(line)
        self.serial.wait_for("[scon] $ " + line, 20 * self.slow, pos)
        self.nap(0.3)
        return pos

    def repaints(self):
        """(full repaints, loop iterations) of the desktop since boot, from the `repaints`
        shell command; None when the image does not have it."""
        pos = self.serial.mark()
        self.serial.send_line("repaints")
        end = time.time() + 20 * self.slow
        while time.time() < end:
            m = re.search(r"\[scon\] desktop: (\d+) full repaints, (\d+) loop iterations", self.serial.since(pos))
            if m:
                return int(m.group(1)), int(m.group(2))
            time.sleep(0.05)
        return None

    def drag(self, x0, y0, x1, y1, steps=8):
        self.actions.write(f"drag {int(x0)} {int(y0)} {int(x1)} {int(y1)}\n")
        self.move(x0, y0)
        self.nap(0.12)
        self.button(True)
        self.nap(0.3)
        for i in range(1, steps + 1):
            self.move(x0 + (x1 - x0) * i / steps, y0 + (y1 - y0) * i / steps)
            self.nap(0.05)
        self.nap(0.1)
        self.button(False)
        self.nap(0.3)

    def key(self, *qcodes, hold=0.08):
        """Press the keys in order, release in reverse (a chord: key('alt', 'tab'))."""
        self.actions.write("key " + "+".join(qcodes) + "\n")
        k = lambda q, down: {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": q}}}
        for q in qcodes:
            self._send([k(q, True)])
            time.sleep(0.03 * self.slow)
        self.nap(hold)
        for q in reversed(qcodes):
            self._send([k(q, False)])
            time.sleep(0.03 * self.slow)
        self.nap(0.08)

    def type(self, text):
        self.actions.write(f"type {text!r}\n")
        k = lambda q, down: {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": q}}}
        for ch in text:
            qc, shift = self.layout.get(ch, (None, False))
            if qc is None:
                continue
            self._send(([k("shift", True)] if shift else []) + [k(qc, True)])
            self.nap(0.05)
            self._send([k(qc, False)] + ([k("shift", False)] if shift else []))
            self.nap(0.05)

    # ── screen ──────────────────────────────────────────────────────────────
    def shot(self, save_as=None):
        self.shot_n += 1
        ppm = os.path.join(self.wd, "screen.ppm")
        r = self.qmp.cmd("screendump", filename=ppm)
        if "error" in r:
            raise Fatal(f"screendump: {r['error']}")
        s = Screen(ppm)
        if save_as:
            s.save_png(os.path.join(self.wd, save_as + ".png"))
        return s

    def look(self, save_as=None):
        """A screendump for finding windows: the pointer is parked in the bottom
        right corner first, so it cannot sit on a title-bar button."""
        self.move(self.w - 4, self.h - 4)
        self.nap(0.25)
        return self.shot(save_as)

    def desktop_region(self):
        """The part of the screen windows live in (not the dock, not the clock)."""
        return (SIDEBAR_W, 0, self.w - PANEL_W - 20, self.h)

    def close_all_windows(self):
        for _ in range(16):
            wins = self.look().windows()
            if not wins:
                return True
            x, y = wins[-1]
            self.click(x + 20, y + 22)
            self.nap(0.4)
        return not self.look().windows()

    # ── smoke pass ──────────────────────────────────────────────────────────
    def step(self, name, cond, detail=""):
        """Record a step and check the guest is still alive after it."""
        self.record(name, bool(cond), detail)
        self.alive(name)
        return bool(cond)

    def terminal_echo(self, tag):
        """Type a command in the focused terminal; it must show up on serial."""
        # Whatever is already on the prompt line (the monkey types without Enter
        # half of the time) is sent off first, so the command starts a line.
        self.key("ret")
        self.nap(0.3)
        pos = self.serial.mark()
        self.type("echo " + tag)
        self.key("ret")
        return self.serial.wait_for("[e2e] $ echo " + tag, 20 * self.slow, pos)

    def use_app(self, name, x, y):
        """A few realistic actions inside the window at (x, y)."""
        bx, by = x + 40, y + TITLEBAR_H + 30          # inside the body
        if name == "terminal":
            self.click(x + 300, y + 200)
            ok = self.terminal_echo("gui-smoke-term")
            self.step("terminal: typed command runs (keyboard -> shell -> serial)", ok,
                      "" if ok else "no '[e2e] $ echo gui-smoke-term' on serial: keys lost or wrong layout")
            self.type("ls /")
            self.key("ret")
            self.type("uname")
            self.key("ret")
        elif name == "files":
            self.click(bx + 200, by + 90)
            self.key("down"); self.key("down"); self.key("up")
            self.click(bx + 200, by + 130, double=True)
            self.nap(0.5)
            self.click(bx + 260, by + 200, button="right")       # context menu in the list
            self.nap(0.4)
            self.key("esc")
            self.click(bx + 420, by + 320)
        elif name == "notes":
            self.click(bx + 300, by + 150)
            self.type("note from gui-smoke 123")
            self.key("ret")
            self.type("second line")
            self.key("backspace"); self.key("backspace")
        elif name == "text":
            self.click(bx + 200, by + 120)
            self.type("hello from gui-smoke")
            self.key("ret")
            self.type("line two: 42")
            self.key("left"); self.key("left"); self.key("backspace"); self.key("end")
            self.click(x + 60, y + TITLEBAR_H + 20)               # toolbar
        elif name == "browser":
            self.click(x + 320, y + TITLEBAR_H + 58)              # address bar
            self.type("about")
            self.key("backspace")
            self.key("esc")
            self.click(x + 320, y + TITLEBAR_H + 58)
            self.type("gui-smoke.invalid/")                     # a name that resolves nowhere
            self.key("ret")
            self.nap(1.0)
            self.click(bx + 300, by + 250)
        elif name == "settings":
            for k in range(5):
                self.click(x + 70, y + TITLEBAR_H + 40 + k * 44)
            self.click(bx + 420, by + 160)
            self.key("tab")
        elif name == "store":                                      # EuroReken, the calculator
            for k in range(4):
                self.click(bx + 60 + k * 70, by + 180)
            self.type("12+30")
            self.key("ret")
        elif name == "star":                                       # EuroAgent dispatch panel
            self.click(bx + 300, by + 80)
            self.type("list files")
            self.key("ret")
            self.nap(1.0)
        else:                                                      # clock, monitor, log
            self.click(bx + 200, by + 120)
            self.click(bx + 320, by + 220)
            self.key("down"); self.key("pgdn"); self.key("up")
        self.nap(0.3)

    def smoke(self):
        self.shot("00-boot")
        ok = self.close_all_windows()
        # With the install wizard open at boot (a blank disk attached) the tour waits
        # for that window to be closed: it comes up now. Esc closes it.
        log = self.serial.since(0)
        if "[tour] waiting: the install wizard is open" in log and "[tour] closed" not in log:
            shown = self.serial.wait_for("[tour] first-run tour shown", 10 * self.slow)
            pos = self.serial.mark()
            self.key("esc")
            closed = self.serial.wait_for("[tour] closed", 20 * self.slow, pos)
            self.record("first-run tour: waits for the install wizard, shown when it is closed, Esc closes it", shown and closed)
        empty = self.look("01-empty-desktop")
        self.step("close every window the desktop boots with", ok and not empty.windows())
        region = self.desktop_region()
        for i, name in enumerate(DOCK_APPS):
            before = self.look()
            self.click(*dock_tile(i))
            self.nap(0.8)
            s = self.look()
            wins = s.windows()
            opened = bool(wins) and s.differs(before, region=region)
            if not self.step(f"{name}: dock tile opens a window", opened,
                             "" if opened else f"{len(wins)} window(s) found, screen changed={s.differs(before, region=region)}"):
                s.save_png(os.path.join(self.wd, f"app-{i:02d}-{name}-FAILED.png"))
                self.close_all_windows()
                continue
            x, y = wins[0]
            self.use_app(name, x, y)
            self.alive(f"{name}: use")
            used = self.look(f"app-{i:02d}-{name}")
            self.record(f"{name}: used (clicks, typing, keys)", True, f"window at {x},{y}; screenshot app-{i:02d}-{name}.png")
            # Using an app may open another window (a file from EuroFiles): work on the
            # one we opened if it is still there.
            wins = used.windows()
            if (x, y) not in wins and wins:
                x, y = min(wins, key=lambda w: abs(w[0] - x) + abs(w[1] - y))
            # Move it by its title bar and look where it went.
            dx, dy = (70, 50) if x + 70 + 400 < self.w - PANEL_W else (-70, 50)
            self.drag(x + 190, y + 22, x + 190 + dx, y + 22 + dy)
            moved = self.look().windows()
            # The window must have followed the pointer: at least half of the way in
            # both directions and not past it. (Under TCG the guest latches the press
            # a few pixels into the drag, so the grab offset is not the click point.)
            def followed(w):
                mx, my = (w[0] - x) * (1 if dx > 0 else -1), w[1] - y
                return abs(dx) / 2 <= mx <= abs(dx) + 12 and dy / 2 <= my <= dy + 12
            near = [w for w in moved if followed(w)]
            self.step(f"{name}: window moves with a title-bar drag", bool(near),
                      f"{x},{y} -> {near[0][0]},{near[0][1]}" if near else f"expected about {x + dx},{y + dy}, windows now {moved}")
            if near:
                x, y = near[0]
            self.click(x + 20, y + 22)                              # the close button
            self.nap(0.5)
            left = self.look().windows()
            closed = not any(abs(w[0] - x) <= 12 and abs(w[1] - y) <= 12 for w in left)
            self.step(f"{name}: close button closes the window", closed, "" if closed else f"still at {x},{y}")
            self.close_all_windows()
        self.desktop_features()

    def desktop_features(self):
        region = self.desktop_region()
        # Three windows, then the Alt-Tab switcher.
        for i in (0, 8, 4):
            self.click(*dock_tile(i))
            self.nap(0.6)
        three = self.look()
        self.step("three apps open together", len(three.windows()) >= 2, f"{len(three.windows())} title bars visible")
        k = lambda q, down: {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": q}}}
        self.actions.write("alt-tab x2\n")
        self._send([k("alt", True)])
        self.nap(0.1)
        for _ in range(2):
            self._send([k("tab", True)]); self.nap(0.1); self._send([k("tab", False)]); self.nap(0.3)
        sw = self.shot("feature-alt-tab")
        self._send([k("alt", False)])
        self.nap(0.5)
        after = self.look()
        shown = sw.differs(three, min_pixels=15000, region=region)
        gone = after.differs(sw, min_pixels=15000, region=region)
        self.step("Alt-Tab shows the switcher, releasing Alt closes it and raises a window",
                  shown and gone and bool(after.windows()),
                  f"switcher panel shown={shown}, closed after Alt release={gone}")
        # The same with EuroText in front: an editor reads the keyboard through the
        # kernel's rich-key path (arrows, Ctrl), a terminal through the character path.
        self.close_all_windows()
        for i in (4, 8):
            self.click(*dock_tile(i))
            self.nap(0.6)
        two = self.look()
        self.actions.write("alt-tab (editor in front)\n")
        self._send([k("alt", True)])
        self.nap(0.1)
        self._send([k("tab", True)]); self.nap(0.1); self._send([k("tab", False)]); self.nap(0.3)
        sw2 = self.shot("feature-alt-tab-editor")
        self._send([k("alt", False)])
        self.nap(0.5)
        self.step("Alt-Tab with the text editor in front", sw2.differs(two, min_pixels=15000, region=region))
        self.close_all_windows()
        for i in (0, 4):
            self.click(*dock_tile(i))
            self.nap(0.6)
        # Workspaces: Alt+2 is an empty desktop, Alt+1 brings the windows back.
        self.key("alt", "2")
        self.nap(0.8)
        ws2 = self.look("feature-workspace-2")
        self.key("alt", "1")
        self.nap(0.8)
        ws1 = self.look()
        self.step("workspace switch (Alt+2 empty, Alt+1 windows back)",
                  not ws2.windows() and len(ws1.windows()) >= 1,
                  f"desktop 2: {len(ws2.windows())} windows, back on 1: {len(ws1.windows())}")
        self.close_all_windows()
        # Launcher: open with the EU mark, search, open the top match.
        base = self.shot()
        self.click(*BRAND)
        self.nap(0.6)
        self.type("term")
        self.nap(0.4)
        la = self.shot("feature-launcher-search")
        self.step("launcher opens and takes a search", la.differs(base, min_pixels=2000))
        self.key("ret")
        self.nap(0.8)
        term = self.look()
        wins = term.windows()
        ok, why = bool(wins), "no window after Enter"
        if ok:
            self.click(wins[0][0] + 300, wins[0][1] + 200)
            ok, why = self.terminal_echo("gui-smoke-launcher"), "the typed command did not reach the shell"
        self.step("launcher search 'term' + Enter opens a working terminal", ok, "" if ok else why)
        self.close_all_windows()
        # Context menu on the bare desktop.
        base = self.shot()
        cx, cy = SIDEBAR_W + (self.w - SIDEBAR_W - PANEL_W) // 2, self.h * 3 // 4
        self.click(cx, cy, button="right")
        self.nap(0.6)
        menu = self.shot("feature-context-menu")
        self.step("right click opens a context menu", menu.differs(base, min_pixels=400, region=region))
        self.key("esc")
        self.nap(0.5)
        closed = self.shot()
        self.step("Esc closes the context menu (BUG-057)",
                  closed.differs(menu, min_pixels=400, region=region) and not closed.differs(base, min_pixels=400, region=region))
        self.click(cx - 200, cy - 200)      # an image without that fix: the click dismisses the menu
        self.nap(0.4)
        self.notification_list(cx, cy)
        self.altgr()
        self.mouse_wheel()
        self.close_all_windows()
        self.step("desktop back to rest", not self.look("99-final").windows())

    def notification_list(self, cx, cy):
        """BUG-057: the list opens on the status panel, closes with Esc and with a click
        beside it, and while it is open the desktop repaints when something changes
        instead of in every loop iteration."""
        px, py = self.w - PANEL_W // 2, 60
        self.move(cx, cy)
        self.nap(0.3)
        base = self.shot()
        self.click(px, py)
        self.nap(0.8)
        self.move(cx, cy)
        self.nap(0.5)
        lst = self.shot("feature-notification-list")
        self.step("notification list: a click on the status panel opens it", lst.differs(base, min_pixels=8000))
        r1 = self.repaints()
        self.nap(3.0)
        r2 = self.repaints()
        if r1 and r2:
            d_rep, d_iter = r2[0] - r1[0], r2[1] - r1[1]
            self.step("notification list open: the desktop repaints on change, not in every loop iteration (BUG-057)",
                      d_iter >= 20 and d_rep * 10 <= d_iter, f"{d_rep} full repaints in {d_iter} loop iterations")
        else:
            self.step("notification list open: the desktop repaints on change, not in every loop iteration (BUG-057)",
                      False, "the image has no `repaints` command")
        self.key("esc")
        self.nap(0.6)
        after = self.shot()
        self.step("notification list: Esc closes it (BUG-057)",
                  after.differs(lst, min_pixels=8000) and not after.differs(base, min_pixels=8000))
        self.click(px, py)
        self.nap(0.8)
        self.move(cx, cy)
        self.nap(0.5)
        lst2 = self.shot()
        self.click(cx, cy)                  # beside the list, on the bare desktop
        self.nap(0.6)
        after2 = self.shot()
        self.step("notification list: a click beside it closes it (BUG-057)",
                  lst2.differs(base, min_pixels=8000) and after2.differs(lst2, min_pixels=8000)
                  and not after2.differs(base, min_pixels=8000))
        if after2.differs(base, min_pixels=8000):       # still open on an image without the fix
            self.click(px, py)
            self.nap(0.5)

    def altgr(self):
        """BUG-052: AltGr gives the third level of the Belgian, French and German layouts
        (the characters arrive in a Terminal command) and is never Alt: AltGr+Tab opens no
        switcher and AltGr+2 leaves the workspace alone, in the Terminal (character path of
        the keyboard driver) and in EuroText (rich-key path)."""
        region = self.desktop_region()
        restore = "be-azerty" if self.layout is AZERTY else "us"
        self.close_all_windows()
        self.click(*dock_tile(4))
        self.nap(0.8)
        wins = self.look().windows()
        if not wins:
            self.step("AltGr: third-level characters on be-azerty, fr-azerty and de-qwertz (BUG-052)", False, "no terminal window")
            return
        self.click(wins[0][0] + 300, wins[0][1] + 200)
        got = []
        for tag, keys in ALTGR_KEYS.items():
            self.scon("keymap " + tag)
            self.key("ret")
            self.nap(0.3)
            pos = self.serial.mark()
            for q in ("e", "c", "h", "o", "spc"):       # the same keys on all four layouts
                self.key(q)
            for q in keys:
                if isinstance(q, tuple):
                    self.key(q[1])
                else:
                    self.key("alt_r", q)
            self.key("ret")
            ok = self.serial.wait_for("[e2e] $ echo " + ALTGR_WANT, 20 * self.slow, pos)
            if not ok:
                line = [l for l in self.serial.since(pos).splitlines() if l.startswith("[e2e] $")]
                got.append(f"{tag}: {line[0] if line else 'no command on serial'}")
        self.step("AltGr: third-level characters on be-azerty, fr-azerty and de-qwertz (BUG-052)", not got, "; ".join(got))
        # AltGr is not Alt. Belgian layout: AltGr+2 is '@', and must not be workspace 2.
        self.scon("keymap be-azerty")
        before = self.look()
        k = lambda q, down: {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": q}}}
        self.actions.write("altgr-tab (terminal in front)\n")
        self._send([k("alt_r", True)])
        self.nap(0.1)
        self._send([k("tab", True)]); self.nap(0.1); self._send([k("tab", False)]); self.nap(0.4)
        held = self.shot("feature-altgr-tab")
        self._send([k("alt_r", False)])
        self.nap(0.3)
        self.key("alt_r", "2")
        self.nap(0.6)
        after = self.look()
        self.step("AltGr+Tab and AltGr+2 in the Terminal: no switcher, no workspace change (BUG-052)",
                  not held.differs(before, min_pixels=15000, region=region) and bool(after.windows()),
                  f"{len(after.windows())} window(s) after AltGr+2")
        self.key("ret")
        self.click(*dock_tile(8))           # EuroText in front: the rich-key path
        self.nap(0.8)
        before = self.look()
        self.actions.write("altgr-tab (editor in front)\n")
        self._send([k("alt_r", True)])
        self.nap(0.1)
        self._send([k("tab", True)]); self.nap(0.1); self._send([k("tab", False)]); self.nap(0.4)
        held = self.shot("feature-altgr-tab-editor")
        self._send([k("alt_r", False)])
        self.nap(0.3)
        self.key("alt_r", "2")
        self.nap(0.6)
        after = self.look()
        self.step("AltGr+Tab and AltGr+2 in EuroText: no switcher, no workspace change (BUG-052)",
                  not held.differs(before, min_pixels=15000, region=region) and bool(after.windows()),
                  f"{len(after.windows())} window(s) after AltGr+2")
        self.scon("keymap " + restore)
        self.close_all_windows()

    def mouse_wheel(self):
        """The wheel scrolls the window under the pointer: EuroBeheer's form, and the
        Terminal's output (a key brings the prompt back)."""
        self.click(*dock_tile(5))
        self.nap(0.8)
        wins = self.look().windows()
        ok, detail = False, "no settings window"
        if wins:
            x, y = wins[0]
            box = (x + 190, y + TITLEBAR_H, x + 750, y + 550)
            self.click(x + 70, y + TITLEBAR_H + 64)       # the System section: longer than the window
            self.nap(0.5)
            self.move(x + 500, y + 300)
            self.nap(0.4)
            before = self.shot()
            self.wheel(3)
            self.nap(0.6)
            down = self.shot("feature-wheel-settings")
            self.wheel(-6)
            self.nap(0.6)
            up = self.shot()
            ok = down.differs(before, min_pixels=1500, region=box) and up.differs(down, min_pixels=1500, region=box)
            detail = "" if ok else "the picture did not change with the wheel"
        self.step("mouse wheel scrolls EuroBeheer down and back up", ok, detail)
        self.close_all_windows()
        self.click(*dock_tile(4))
        self.nap(0.8)
        wins = self.look().windows()
        ok, detail = False, "no terminal window"
        if wins:
            x, y = wins[0]
            box = (x, y + TITLEBAR_H, x + 780, y + 720)
            self.click(x + 300, y + 200)
            self.key("ret")
            self.type("help")
            self.key("ret")
            self.nap(0.8)
            self.move(x + 300, y + 300)
            self.nap(0.4)
            before = self.shot()
            self.wheel(-3)
            self.nap(0.6)
            back = self.shot("feature-wheel-terminal")
            self.key("spc")
            self.nap(0.6)
            prompt = self.shot()
            ok = back.differs(before, min_pixels=1500, region=box) and prompt.differs(back, min_pixels=1500, region=box)
            detail = "" if ok else "the Terminal did not scroll back, or a key did not return to the prompt"
            self.key("backspace")
        self.step("mouse wheel scrolls the Terminal back, a key returns to the prompt", ok, detail)

    # ── monkey ──────────────────────────────────────────────────────────────
    def monkey(self, minutes, seed):
        rng = random.Random(seed)
        chars = sorted(self.layout)
        end = time.time() + minutes * 60
        n = 0
        t0 = time.time()
        counts = {}
        last_shot = None
        frozen = 0
        print(f"[gui-smoke] monkey soak: {minutes} min, seed {seed}", flush=True)
        self.actions.write(f"# monkey seed {seed}\n")
        def rand_point():
            return rng.randrange(self.w), rng.randrange(self.h)
        def win_point():
            wins = self.look().windows()
            if not wins:
                return None
            x, y = rng.choice(wins)
            return x, y
        while time.time() < end:
            n += 1
            r = rng.random()
            self.actions.write(f"# action {n} t={int(time.time() - t0)}s\n")
            if r < 0.12:
                act = "dock"
                self.click(*dock_tile(rng.randrange(len(DOCK_APPS))))
            elif r < 0.31:
                act = "click"
                self.click(*rand_point())
            elif r < 0.33:
                act = "wheel"
                self.move(*rand_point())
                self.wheel(rng.choice([-5, -3, -1, 1, 2, 4, 8]))
            elif r < 0.34:
                act = "altgr"
                self.key("alt_r", rng.choice(["tab", "1", "2", "3", "e", "q", "less", "bracket_left", "esc", "ret"]))
            elif r < 0.38:
                act = "right-click"
                self.click(*rand_point(), button="right")
            elif r < 0.42:
                act = "double-click"
                self.click(*rand_point(), double=True)
            elif r < 0.56:
                act = "click-in-window"
                p = win_point()
                if p:
                    self.click(p[0] + rng.randrange(0, 700), p[1] + rng.randrange(0, 480),
                               button="right" if rng.random() < 0.1 else "left", double=rng.random() < 0.15)
            elif r < 0.62:
                act = "move-window"
                p = win_point()
                if p:
                    self.drag(p[0] + 190, p[1] + 22, *rand_point())
            elif r < 0.66:
                act = "drag"
                self.drag(*rand_point(), *rand_point(), steps=rng.randrange(2, 12))
            elif r < 0.71:
                act = "title-button"
                p = win_point()
                if p:
                    self.click(p[0] + 20 + 20 * rng.randrange(3), p[1] + 22)
            elif r < 0.81:
                act = "type"
                n_chars = rng.randrange(120, 400) if rng.random() < 0.06 else rng.randrange(1, 14)
                self.type("".join(rng.choice(chars) for _ in range(n_chars)))
                if rng.random() < 0.5:
                    self.key("ret")
            elif r < 0.90:
                act = "key"
                for _ in range(rng.randrange(1, 5)):
                    self.key(rng.choice(SPECIAL_KEYS))
            elif r < 0.93:
                act = "alt-tab"
                self.key("alt", "tab")
            elif r < 0.96:
                act = "workspace"
                self.key("alt", str(rng.randrange(1, 5)))
            elif r < 0.985:
                act = "launcher"
                self.click(*BRAND)
                self.type("".join(rng.choice("abcdefghijklmnopqrstuvwxyz") for _ in range(rng.randrange(0, 5))))
                self.key(rng.choice(["ret", "esc", "esc"]))
            elif r < 0.995:
                act = "status-panel"
                self.click(self.w - PANEL_W // 2, 60)
            else:
                act = "ctrl-alt-esc"
                self.key("ctrl", "alt", "esc")
            counts[act] = counts.get(act, 0) + 1
            if self.serial.crash:
                self.fatal(f"monkey action {n} ({act})", "CRASH on serial: " + self.serial.crash)
            if n % 10 == 0:
                self.alive(f"monkey action {n} ({act})")
            if n % 50 == 0:
                # The screen must not be frozen: move the pointer across it and compare.
                # The picture gets up to 5 s to follow. (Until BUG-057 was fixed the
                # desktop repainted everything in every loop iteration while the
                # notification list was open, 0.3 to 0.4 s per frame with a handful of
                # windows, and a single look 0.3 s after the move called that "frozen".)
                self.move(self.w // 2, self.h // 2)
                self.nap(1.0)
                a = self.shot()
                self.move(self.w // 2 + 150, self.h // 2 + 90)
                moved, t_move = False, time.time()
                while not moved and time.time() - t_move < 5 * self.slow:
                    self.nap(0.3)
                    b = self.shot(f"monkey-{(n // 50) % 12:02d}")
                    moved = a.differs(b, min_pixels=4)
                if not moved:
                    frozen += 1
                    if frozen >= 2:
                        self.fatal(f"monkey action {n}", "screen frozen: moving the pointer no longer changes the picture")
                else:
                    frozen = 0
                print(f"[gui-smoke] monkey: {n} actions, {int(time.time() - t0)} s, max console latency {self.max_echo_ms} ms", flush=True)
            if n % 200 == 0:
                # After all that: is it still a desktop that does work? Back to the
                # first workspace, close what can be found, bring the terminal to the
                # front and run a command in it. The terminal is opened through the
                # launcher, which raises and focuses it wherever the monkey left the
                # window. Dragged to the bottom or the right edge it shows less than
                # its three buttons and cannot be found on a screendump, and a dock
                # click on a window that is already in front would close it.
                self.key("esc"); self.key("esc")
                self.key("alt", "1")
                self.nap(0.5)
                self.close_all_windows()
                # A context menu or the notification list the monkey left open takes the
                # next left click (the two Esc above close them since BUG-057; an older
                # image needs the click): spend one on the bare strip beside the dock, so
                # the click on the EU mark opens the launcher.
                self.click(SIDEBAR_W + 6, self.h - 6)
                self.nap(0.3)
                self.click(*BRAND)
                self.nap(0.5)
                self.type("term")
                self.nap(0.3)
                self.key("ret")
                self.nap(0.8)
                ok = self.terminal_echo(f"gui-monkey-{n}")
                if not ok:                      # what did the desktop look like when the command did not arrive?
                    self.shot(f"monkey-check-{n}-failed")
                self.record(f"monkey: desktop still works after {n} actions (terminal runs a typed command)", ok)
                self.alive(f"monkey functional check {n}")
        self.alive("monkey end")
        self.key("esc")
        self.key("alt", "1")
        self.shot("monkey-final")
        self.record(f"monkey soak {minutes} min, seed {seed}", True,
                    f"{n} actions ({', '.join(f'{k} {v}' for k, v in sorted(counts.items()))}); max console latency {self.max_echo_ms} ms")

    # ── teardown ────────────────────────────────────────────────────────────
    def stop(self):
        if getattr(self, "serial", None):
            self.serial.closed = True
        if getattr(self, "qemu", None) is None:
            return
        if self.qemu.poll() is None:
            try:
                self.qmp.cmd("quit")
            except Exception:
                pass
            try:
                self.qemu.wait(timeout=10)
            except Exception:
                self.qemu.kill()            # only ever the process this script started
                self.qemu.wait()
        for p in (getattr(self, "qmp_path", ""), getattr(self, "ser_path", ""), os.path.join(self.wd, "screen.ppm")):
            try:
                os.unlink(p)
            except OSError:
                pass
        if not self.a.keep_image:
            try:
                os.unlink(os.path.join(self.wd, "boot.img"))
            except OSError:
                pass


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--image", default="eurokernel.img", help="the image to boot (copied into the work directory)")
    ap.add_argument("--workdir", default="", help="private directory for the image copy, sockets, logs, screenshots (default: a fresh one under $TMPDIR)")
    ap.add_argument("--kvm", action="store_true", help="-enable-kvm -cpu host (default: TCG)")
    ap.add_argument("--cpu-opts", default="", help='appended to -cpu host, e.g. ",host-phys-bits=off,phys-bits=39"')
    ap.add_argument("--smp", type=int, default=2)
    ap.add_argument("--mem", default="2G")
    ap.add_argument("--pin", default="", help="taskset CPU list for QEMU")
    ap.add_argument("--input", choices=["usb", "virtio"], default="usb", help="usb-tablet + usb-kbd on xHCI, or virtio-tablet + virtio-keyboard")
    ap.add_argument("--net", choices=["none", "restrict"], default="none", help="no NIC, or SLIRP with restrict=on")
    ap.add_argument("--extra", default="", help='more QEMU arguments, e.g. "-device intel-hda -device hda-output"')
    ap.add_argument("--layout", choices=["auto", "us", "be-azerty"], default="auto")
    ap.add_argument("--boot-timeout", type=int, default=0, help="seconds to the desktop (default 300 KVM, 1500 TCG)")
    ap.add_argument("--alive-timeout", type=int, default=0, help="seconds the desktop loop gets to answer (default 30 KVM, 180 TCG)")
    ap.add_argument("--slow", type=float, default=0.0, help="input timing factor (default 1 KVM, 3 TCG)")
    ap.add_argument("--soak", type=float, default=0.0, metavar="MINUTES", help="monkey test for this long after the smoke pass")
    ap.add_argument("--seed", type=int, default=None, help="monkey seed (default: from the clock; printed and in the report)")
    ap.add_argument("--skip-smoke", action="store_true", help="monkey only")
    ap.add_argument("--keep-image", action="store_true", help="keep the image copy in the work directory")
    a = ap.parse_args()
    if OVMF is None:
        sys.exit("no OVMF firmware found")
    a.boot_timeout = a.boot_timeout or (300 if a.kvm else 1500)
    a.alive_timeout = a.alive_timeout or (30 if a.kvm else 180)
    a.slow = a.slow or (1.0 if a.kvm else 3.0)
    a.seed = a.seed if a.seed is not None else int(time.time())
    a.workdir = os.path.abspath(a.workdir) if a.workdir else tempfile.mkdtemp(prefix="gui-smoke-")
    os.makedirs(a.workdir, exist_ok=True)
    if os.path.exists(os.path.join(a.workdir, "qmp.sock")):
        sys.exit(f"{a.workdir} has a live qmp.sock: another gui-smoke uses this work directory")
    g = Guest(a)
    verdict, rc = "PASS", 0
    t0 = time.time()
    try:
        g.boot()
        g.alive("boot")
        if not a.skip_smoke:
            g.smoke()
        if a.soak > 0:
            g.monkey(a.soak, a.seed)
    except Fatal as e:
        verdict, rc = f"CRASH/HANG: {e}", 2
    except KeyboardInterrupt:
        verdict, rc = "interrupted", 1
    finally:
        g.stop()
    failed = [r for r in g.results if not r[1]]
    if rc == 0 and failed:
        verdict, rc = f"FAIL ({len(failed)} step(s))", 1
    report = {
        "verdict": verdict, "image": os.path.abspath(a.image), "accel": "kvm" if a.kvm else "tcg",
        "smp": a.smp, "mem": a.mem, "input": a.input, "net": a.net, "seed": a.seed, "soak_minutes": a.soak,
        "seconds": int(time.time() - t0), "max_console_latency_ms": g.max_echo_ms,
        "known_issues": {n: KNOWN_ISSUES[n] for n in g.known},
        "steps": [{"name": n, "ok": ok, "detail": d} for n, ok, d in g.results],
    }
    with open(os.path.join(a.workdir, "report.json"), "w") as f:
        json.dump(report, f, indent=1)
    with open(os.path.join(a.workdir, "report.txt"), "w") as f:
        f.write(f"gui-smoke: {verdict}\n{report['accel']} smp={a.smp} mem={a.mem} input={a.input} net={a.net} seed={a.seed} soak={a.soak} min, {report['seconds']} s, max console latency {g.max_echo_ms} ms\n\n")
        for n, ok, d in g.results:
            f.write(f"{'known' if n in g.known else 'ok  ' if ok else 'FAIL'} {n}{': ' + d if d else ''}\n")
    known = f", {len(g.known)} known issue(s): {', '.join(sorted(set(KNOWN_ISSUES[n] for n in g.known)))}" if g.known else ""
    print(f"[gui-smoke] {verdict} — {len(g.results) - len(failed)}/{len(g.results)} steps ok{known}, report and screenshots in {a.workdir}", flush=True)
    sys.exit(rc)


if __name__ == "__main__":
    main()
