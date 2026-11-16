#!/usr/bin/env python3
"""Chromium on the EuroOS desktop: smoke test, site matrix, interaction matrix, soak.

Boots an image under QEMU with the Chromium EuroPack and the NSS EuroPack attached,
starts `chrome` from the EuroOS Terminal and works the browser the way a person does:
a USB tablet and a USB keyboard injected over QMP (the Guest class of gui-smoke.py).
Nothing is sent to the page over DevTools. What the browser shows is judged from

  * screendumps (the content area: how many colours, did it change, is it stable), and
  * the serial console: `ctabs` (a shell command) prints every open page with its URL
    and title, chrome's own FATAL lines and the kernel's fault lines are counted, and
    the input test page (/tmp/input-test.html in the guest) mirrors its form fields,
    the last clipboard event and the scroll position into its title.

  python3 scripts/chrome-smoke.py --image eurokernel.img --pack chrome-pack2.img \\
      --nss nss-pack.img --kvm --workdir /srv/euroos/chrome-runs/smoke-1
  ... --only sites            the site matrix alone (also: interact, youtube, localvideo, consent, perf, starts)
  ... --only localvideo --media-dir DIR   H.264/AAC and VP9/Opus clips (scripts/mk-test-video.sh) served to the
                              guest from this host at http://10.0.2.2:PORT/ (never to the outside)
  ... --soak-hours 2          repeat sites + interaction for that long, memory per round
  ... --starts 20             boot, start chrome, type a URL in the omnibox: 20 fresh boots

The guest needs the internet for the site matrix, so QEMU's user-mode network runs
WITHOUT restrict=on here (gui-smoke keeps it on). Use an image built with the boot
option update.check=off, visit only the sites of the matrix, log in nowhere.

Exit status: 0 = every step passed, 1 = a step failed, 2 = the guest crashed or hung.
Details: docs/CHROME-ON-EUROOS.md ("Testing"), docs/GUI-TESTING.md.
"""
import argparse, hashlib, http.server, urllib.parse, importlib.util, json, os, re, shlex, socketserver, statistics, struct, sys, threading, time, types, wave

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("gui_smoke", os.path.join(HERE, "gui-smoke.py"))
gs = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gs)

# Chromium's own UI in window pixels (Chromium 152, default theme, scale factor 1).
TOOLBAR_Y = 62
BACK_X, FORWARD_X, RELOAD_X, OMNIBOX_X = 22, 58, 95, 350
CONTENT_TOP, INFOBAR_H = 87, 56
# The input test page (kernel/src/input_test_page.html), in page pixels.
FIELD_A, FIELD_B, WORD = (300, 105), (300, 183), (50, 237)
SENTENCE = "select-this-word and copy it"

# The site matrix (docs/SPRINT-PLAN-CHROME-WORKPLACE.md). `host` must be in the URL the
# browser ends on; `colours` is the least number of distinct colours a real rendering of
# that page has in the content area (an error page or a blank one has a few dozen).
SITES = [
    {"name": "euro-os", "url": "https://euro-os.eu/", "host": "euro-os.eu", "colours": 400},
    {"name": "wikipedia", "url": "https://nl.wikipedia.org/", "host": "wikipedia.org", "colours": 300},
    {"name": "github", "url": "https://github.com/GoTrustbe/Euroos", "host": "github.com", "colours": 150},
    {"name": "youtube", "url": "https://www.youtube.com/", "host": "youtube.com", "colours": 60, "consent_expected": True},
    {"name": "openstreetmap", "url": "https://www.openstreetmap.org/", "host": "openstreetmap.org", "colours": 300},
    {"name": "pdf", "url": "https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf", "host": "w3.org", "colours": 8, "consent": False},
]
VIDEO_URL = "https://www.youtube.com/watch?v=aqz-KE-bpKQ"      # Big Buck Bunny, 10 minutes
# Public videos tried in turn when YouTube puts its "confirm you're not a bot" page in front
# of one of them (every one is reported; nothing is done about the page itself).
VIDEO_URLS = [VIDEO_URL, "https://www.youtube.com/watch?v=YE7VzlLtp-4", "https://www.youtube.com/watch?v=eRsGyueVLvQ",
              "https://www.youtube.com/watch?v=R6MlUcmOul8"]

# Consent banners, found without looking at the DOM: Chromium's own find-in-page (Ctrl+F, typed
# through the X server like everything else) paints the ACTIVE match orange, wherever the text
# is (page, overlay, iframe, scrolled out of sight: find scrolls it in). The harness types a
# label, looks for those orange pixels, closes the find bar with Esc and CLICKS the spot with the
# tablet. Order: reject first, then accept (no reject button on the banner).
CONSENT_REJECT = ["reject all", "alles weigeren", "tout refuser", "alle ablehnen", "decline all", "reject", "weigeren", "refuser", "ablehnen"]
CONSENT_ACCEPT = ["accept all", "alles accepteren", "tout accepter", "alle akzeptieren", "accept", "agree", "accepteren", "akzeptieren"]
CONSENT_RETRY = ("reject all", "alles weigeren", "tout refuser", "alle ablehnen")
FIND_ACTIVE = bytes((255, 150, 50))       # Blink's active find match background, 0xFF9632

# Characters beyond the tables of gui-smoke: (qcode, shift, altgr).
US_EXTRA = {"#": ("3", True, False), "%": ("5", True, False), "@": ("2", True, False), "'": ("apostrophe", False, False),
            "[": ("bracket_left", False, False), "]": ("bracket_right", False, False), "{": ("bracket_left", True, False),
            "}": ("bracket_right", True, False), "\\": ("backslash", False, False), "|": ("backslash", True, False),
            "~": ("grave_accent", True, False), "$": ("4", True, False), "<": ("comma", True, False), ">": ("dot", True, False)}
AZ_EXTRA = {"@": ("2", False, True), "#": ("3", False, True), "{": ("9", False, True), "}": ("0", False, True),
            "[": ("bracket_left", False, True), "]": ("bracket_right", False, True), "€": ("e", False, True),
            "|": ("1", False, True), "'": ("4", False, False), "\"": ("3", False, False), "<": ("less", False, False),
            ">": ("less", True, False), "é": ("2", False, False), "è": ("7", False, False),
            "ç": ("9", False, False), "à": ("0", False, False), "%": ("apostrophe", True, False),
            "$": ("bracket_right", False, False), "*": ("backslash", False, False)}

BAD_RE = re.compile(r":FATAL:|\[abort\]|TERMINATED|arena alloc FAILED|POOL EXHAUSTED|WATCHDOG TRIPPED")
# The desktop loop's deadman ("WATCHDOG TRIPPED", 15 s without a turn of the loop,
# BUG-085) only writes a line and the guest goes on: here it fails the step it
# happens in (BAD_RE) instead of ending the run as a crash.
gs.CRASH_RE = re.compile(gs.CRASH_RE.pattern.replace("|WATCHDOG", ""))


class EchoProbe(threading.Thread):
    """The console echo of gui-smoke, all the time: four times a second the serial
    console is asked to echo a token and the answer is timed. The desktop loop
    answers, so the slowest answer in a stretch of time is how long the loop stood
    still in it (plus the few milliseconds an answer always takes)."""

    def __init__(self, guest, period=0.25, nmi_after=2.0, nmi_max=12):
        super().__init__(daemon=True)
        self.g, self.period = guest, period
        self.samples = []               # (time sent, ms until the answer; None = not yet)
        self.lock = threading.Lock()
        self.halt = False
        # An answer that takes longer than `nmi_after` seconds: an NMI, and one
        # more for every three further seconds the loop stands (the kernel's NMI probe
        # prints the interrupted RIP, the stack and every task: the serial log
        # then says WHERE it stood; scripts/nmisym.py names the addresses).
        self.nmi_after, self.nmi_left, self.nmis = nmi_after, nmi_max, []

    def run(self):
        n = 0
        while not self.halt and not self.g.serial.crash:
            n += 1
            token = f"cs-probe-{n}"
            pos = self.g.serial.mark()
            t0 = time.time()
            with self.lock:
                self.samples.append([t0, None])
            try:
                self.g.serial.send_line("echo " + token)
            except OSError:
                return
            next_nmi = self.nmi_after
            while not self.halt and time.time() - t0 < 120:
                if "[scon] " + token in self.g.serial.since(pos):
                    break
                if self.nmi_left > 0 and time.time() - t0 > next_nmi:
                    next_nmi += 3.0
                    self.nmi_left -= 1
                    try:
                        self.g.qmp.cmd("inject-nmi")
                        self.nmis.append(round(time.time() - t0, 2))
                    except Exception:
                        pass
                time.sleep(0.004)
            with self.lock:
                self.samples[-1][1] = (time.time() - t0) * 1000
            time.sleep(self.period)

    def worst(self, since=0.0):
        """(slowest answer in ms, number of answers) among the echoes sent after `since`;
        an echo that is still waiting counts with the time it has waited so far."""
        now = time.time()
        with self.lock:
            ms = [(m if m is not None else (now - t) * 1000) for t, m in self.samples if t >= since]
        return (max(ms), len(ms)) if ms else (0.0, 0)

    def median(self, since=0.0):
        with self.lock:
            ms = sorted(m for t, m in self.samples if t >= since and m is not None)
        return statistics.median(ms) if ms else 0.0


# A desktop that stands still this long during a step fails the step. The sprint's
# goal is 100 ms; the number itself is in every step's detail and in the report.
STALL_FAIL_MS = 1000


class Chrome(gs.Guest):
    """The gui-smoke guest plus what a browser test needs."""

    def __init__(self, a):
        super().__init__(a)
        self.win = None                 # (x, y) of the Chromium frame on the screen
        self.xw, self.xh = 800, 600     # chrome's X window (= the frame's body)
        self.infobars = 0
        self.bad_pos = 0
        self.layout_name = "us"
        self.probe = None               # EchoProbe, from the first desktop on
        self.stalls = []                # (step, slowest console answer in ms)
        self.consent_log = []           # one dict per consent button that was clicked
        self.find_misses = 0            # a full reject label found nothing (then typed again, slowly)
        self.find_retries = []          # (label, did the second attempt find it)

    def start_probe(self):
        if self.probe is None:
            # The probe sends its NMI over the same QMP connection as the input
            # events: one command at a time.
            raw, lock = self.qmp.cmd, threading.Lock()

            def locked(execute, **arguments):
                with lock:
                    return raw(execute, **arguments)
            self.qmp.cmd = locked
            self.probe = EchoProbe(self)
            self.probe.start()

    def deskstat(self):
        """The kernel's own stopwatch on its desktop loop (`deskstat`): (longest
        gap between two passes in ms, gaps over 100 ms) since the last call."""
        pos = self.serial.mark()
        self.serial.send_line("deskstat")
        if not self.serial.wait_for("[deskstat] deskstat:", 10 * self.slow, pos):
            return None
        out = self.serial.since(pos)
        m = re.search(r"\[deskstat\] deskstat: longest time between two passes of the desktop loop (\d+) ms, (\d+) over", out)
        q = re.search(r"its console output waited at most (\d+) ms", out)
        return (int(m.group(1)), int(m.group(2)), int(q.group(1)) if q else -1) if m else None

    def diag_stuck(self, why):
        """A step the browser did not follow: what every thread of every Chromium
        process is doing (`cdiag` with the full console for a moment) and an NMI
        census of every task, for the first two such steps of a run."""
        self.stuck_dumps = getattr(self, "stuck_dumps", 0) + 1
        if self.stuck_dumps > 2:
            return
        print(f"[chrome-smoke]   {why}: cdiag and an NMI census on the serial log", flush=True)
        for line in ("chatter on", "cdiag"):
            self.serial.send_line(line)
            self.nap(1.0)
        self.nap(3.0)
        self.serial.send_line("chatter off")
        self.nap(0.5)
        try:
            self.qmp.cmd("inject-nmi")
        except Exception:
            pass
        self.nap(3.0)

    def ramfiles(self):
        """The `ramfiles` shell command: its first line, or None."""
        pos = self.serial.mark()
        self.serial.send_line("ramfiles")
        if not self.serial.wait_for("[ramfiles] ramfiles:", 10 * self.slow, pos):
            return None
        m = re.search(r"\[ramfiles\] ramfiles: (\d+) files hold (\d+) KiB of bytes and (\d+) KiB of holes", self.serial.since(pos))
        return tuple(int(x) for x in m.groups()) if m else None

    def stall(self, name, since):
        """The slowest console answer since `since`, noted under `name`."""
        if self.probe is None:
            return 0.0
        ms, n = self.probe.worst(since)
        self.stalls.append((name, round(ms)))
        return ms

    def fds(self):
        """The `fds` shell command: (processes, descriptors, objects, file objects in use) or None."""
        pos = self.serial.mark()
        self.serial.send_line("fds")
        if not self.serial.wait_for("[fds] fds:", 10 * self.slow, pos):
            return None
        m = re.search(r"\[fds\] fds: (\d+) processes, (\d+) descriptors, (\d+) objects; (\d+) of (\d+)", self.serial.since(pos))
        return tuple(int(x) for x in m.groups()) if m else None

    # ── input ───────────────────────────────────────────────────────────────
    def type(self, text):
        """Like gui-smoke's, with AltGr and the characters its tables do not have."""
        self.actions.write(f"type {text!r}\n")
        k = lambda q, down: {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": q}}}
        extra = AZ_EXTRA if self.layout_name == "be-azerty" else US_EXTRA
        for ch in text:
            if ch in self.layout and ch not in extra:
                qc, shift = self.layout[ch]
                altgr = False
            elif ch in extra:
                qc, shift, altgr = extra[ch]
            else:
                raise gs.Fatal(f"chrome-smoke cannot type {ch!r} on {self.layout_name}")
            mods = (["shift"] if shift else []) + (["alt_r"] if altgr else [])
            self._send([k(m, True) for m in mods] + [k(qc, True)])
            self.nap(0.04)
            self._send([k(qc, False)] + [k(m, False) for m in reversed(mods)])
            self.nap(0.04)

    def set_layout(self, name):
        """Switch the guest's keyboard layout (the `keymap` shell command) and our table."""
        self.scon("keymap " + name)
        self.layout = gs.AZERTY if name == "be-azerty" else gs.US
        self.layout_name = name
        self.nap(0.8)                   # MappingNotify, and the client fetches the mapping

    def step(self, name, cond, detail=""):
        """A failed step also says what the X server's input state was (a stuck
        modifier or button explains a shortcut that did nothing) and keeps the picture."""
        if not cond and self.win:
            pos = self.serial.mark()
            self.serial.send_line("cdiag")
            self.serial.wait_for("[xin] state:", 10 * self.slow, pos)
            m = re.search(r"\[xin\] state: (.*)", self.serial.since(pos))
            detail += " | X input " + (m.group(1).strip() if m else "state unknown")
            self.fail_n = getattr(self, "fail_n", 0) + 1
            try:
                self.shot(f"fail-{self.fail_n:02d}")
            except gs.Fatal:
                pass
        return super().step(name, cond, detail)

    # ── geometry ────────────────────────────────────────────────────────────
    def cx(self, x, y):
        """A point of chrome's window (x < 0: from the right edge) on the screen."""
        wx, wy = self.win
        return wx + (x if x >= 0 else self.xw + x), wy + gs.TITLEBAR_H + y

    def page(self, x, y):
        """A point of the PAGE (content area) on the screen."""
        return self.cx(x, CONTENT_TOP + INFOBAR_H * self.infobars + y)

    def content(self):
        """The content area as a screen rectangle (x0, y0, x1, y1), without the scrollbar."""
        x0, y0 = self.page(0, 0)
        x1, y1 = self.cx(self.xw - 16, self.xh)
        return x0, y0, min(x1, self.w), min(y1, self.h)

    def track_size(self):
        """Follow the kernel's own account of the X window's size."""
        m = re.findall(r"\[xserver\] hosted window 0x[0-9a-f]+ resized to (\d+)x(\d+) by the desktop", self.serial.since(0))
        if m:
            self.xw, self.xh = int(m[-1][0]), int(m[-1][1])

    def find_chrome(self, known=()):
        """The frame that was not on the screen before chrome started."""
        for w in self.look().windows():
            if not any(abs(w[0] - k[0]) < 8 and abs(w[1] - k[1]) < 8 for k in known):
                return w
        return None

    # ── what the browser says ───────────────────────────────────────────────
    def tabs(self, timeout=10):
        """[(url, title)] of every open page, from the `ctabs` shell command."""
        pos = self.serial.mark()
        self.serial.send_line("ctabs")
        end = time.time() + timeout * self.slow
        while time.time() < end:
            out = self.serial.since(pos)
            if re.search(r"\[tabs\] \d+ page\(s\)", out):
                pages = []
                for url, title in re.findall(r'\[tabs\] page url="((?:[^"\\]|\\.)*)" title="((?:[^"\\]|\\.)*)"', out):
                    try:
                        pages.append((json.loads('"' + url + '"'), json.loads('"' + title + '"')))
                    except ValueError:
                        pages.append((url, title))
                return pages
            if "ctabs: no hosted browser" in out:
                return []
            time.sleep(0.05)
        return None

    def title(self):
        t = self.tabs()
        return t[0][1] if t else ""

    def wait_tab(self, pred, timeout):
        """Poll `ctabs` until pred(url, title) holds for a page; returns (seconds, url, title) or None."""
        t0 = time.time()
        while time.time() - t0 < timeout * self.slow:
            for url, title in self.tabs() or []:
                if pred(url, title):
                    return time.time() - t0, url, title
            time.sleep(0.25)
        return None

    def page_state(self):
        """(A, B, clip, y) of the input test page, whichever tab it is in."""
        for url, title in self.tabs() or []:
            m = re.search(r"A=\[(.*)\] B=\[(.*)\] clip=(\S+) y=(-?\d+)", title)
            if m:
                return m.group(1), m.group(2), m.group(3), int(m.group(4))
        return None

    def field(self, name):
        st = self.page_state()
        return None if st is None else st[0 if name == "A" else 1]

    def wait_state(self, pred, timeout=6):
        """page_state() until pred(state) holds: the title follows a key by a moment
        (the renderer, the browser, then the DevTools answer). Returns the last state."""
        t0, st = time.time(), None
        while time.time() - t0 < timeout * self.slow:
            st = self.page_state()
            if st and pred(st):
                return st
            time.sleep(0.2)
        return st

    def scroll_settled(self, timeout=8):
        """scrollY once the page has stopped scrolling (two reads 0.6 s apart agree): Chromium scrolls
        smoothly, and the first change of the title is only the start of the movement. The next key
        of a step is sent after this, not in the middle of the previous one's animation."""
        last, t0 = None, time.time()
        while time.time() - t0 < timeout * self.slow:
            st = self.page_state()
            y = st[3] if st else None
            if y is not None and y == last:
                return y
            last = y
            time.sleep(0.6)
        return last if last is not None else -1

    def new_bad(self):
        """Lines since the last call in which chrome or the kernel reports a failure."""
        text = self.serial.since(self.bad_pos)
        self.bad_pos = self.serial.mark()
        return [l.strip()[:200] for l in text.splitlines() if BAD_RE.search(l)]

    # ── the picture ─────────────────────────────────────────────────────────
    def colours(self, s, region=None, step=3):
        """Distinct colours and the share of the commonest one in a screen region."""
        x0, y0, x1, y1 = region or self.content()
        seen = {}
        for y in range(y0, y1, step):
            row = s.rgb[(y * s.w + x0) * 3:(y * s.w + x1) * 3]
            for i in range(0, len(row) - 2, 3 * step):
                c = row[i:i + 3]
                seen[c] = seen.get(c, 0) + 1
        total = sum(seen.values()) or 1
        return len(seen), max(seen.values(), default=0) / total

    def digest(self, s, region=None):
        x0, y0, x1, y1 = region or self.content()
        h = hashlib.sha1()
        for y in range(y0, y1):
            h.update(s.rgb[(y * s.w + x0) * 3:(y * s.w + x1) * 3])
        return h.digest()

    def settle(self, timeout, quiet=1.5, region=None, save_as=None):
        """Screendumps until the region has not changed for `quiet` seconds (or the time is
        over). Returns (screen, seconds until the final picture first appeared, settled?)."""
        t0 = time.time()
        last, since, first_seen = None, t0, {}
        while True:
            s = self.shot()
            now = time.time()
            d = self.digest(s, region)
            first_seen.setdefault(d, now)
            if d != last:
                last, since = d, now
            if now - since >= quiet * self.slow or now - t0 >= timeout * self.slow:
                if save_as:
                    s.save_png(os.path.join(self.wd, save_as + ".png"))
                return s, first_seen[d] - t0, now - since >= quiet * self.slow
            time.sleep(0.05)

    # ── chrome ──────────────────────────────────────────────────────────────
    def desktop_ready(self):
        """Close the install wizard, focus the Terminal."""
        s = self.look()
        for w in s.windows():
            if abs(w[0] - (gs.SIDEBAR_W + 16)) <= 3:          # the install wizard (main.rs)
                self.click(w[0] + 20, w[1] + 22)
                self.nap(0.6)
        wins = self.look().windows()
        if not wins:
            self.fatal("desktop", "no Terminal window on the first desktop")
        self.term = wins[0]
        return wins

    def start_chrome(self, url):
        """Type `chrome URL` in the Terminal; wait for the window and the first page."""
        known = self.desktop_ready()
        self.click(self.term[0] + 300, self.term[1] + 300)
        self.key("ret")
        self.nap(0.3)
        pos = self.serial.mark()
        self.bad_pos = pos
        t0 = time.time()
        self.type("chrome " + url)
        self.key("ret")
        ok = self.serial.wait_for("[e2e] chrome: launched", 30 * self.slow, pos)
        if not ok:
            self.record("chrome starts from the Terminal", False, "no '[e2e] chrome: launched' on the serial console")
            return False
        # Where the start-up goes: the moment each of these first shows on the serial console.
        marks = {"launched": "[e2e] chrome: launched", "X connection": "[x11] client connects",
                 "first child process": "[fork] pid", "renderer": "is --type=renderer", "DevTools attached": "[cdp] input bridge attached"}
        seen = {}
        end = time.time() + 90 * self.slow
        while time.time() < end and not (self.win and len(seen) == len(marks)):
            text = self.serial.since(pos)
            for k, needle in marks.items():
                if k not in seen and needle in text:
                    seen[k] = time.time() - t0
            if not self.win:
                self.win = self.find_chrome(known)
                if self.win:
                    seen["window on screen"] = time.time() - t0
            time.sleep(0.2)
        self.start_marks = seen
        if not self.win:
            self.record("chrome starts from the Terminal", False, "no Chromium frame appeared on the screen")
            return False
        hit = self.wait_tab(lambda u, t: t != "" and not u.startswith("about:"), 120)
        s, _, _ = self.settle(40, save_as="chrome-started")
        n, share = self.colours(s)
        secs = time.time() - t0
        ok = hit is not None and n >= 8          # the test page: two flat colours and its text
        timeline = ", ".join(f"{k} {v:.1f}" for k, v in sorted(self.start_marks.items(), key=lambda kv: kv[1]))
        self.move(self.w - 4, self.h - 4)
        self.nap(0.5)
        self.calibrate_idle_icon()
        self.start_probe()
        self.record("chrome starts from the Terminal", ok,
                    f"window at {self.win}, first page {hit[1] if hit else '?'} painted and settled after {secs:.1f} s "
                    f"(seconds after Enter: {timeline}), {n} colours in the content area")
        return ok

    def reload_icon(self, s=None):
        """The pixels of the toolbar's reload button: it shows a cross while a page loads."""
        x, y = self.cx(RELOAD_X, TOOLBAR_Y)
        return self.digest(s or self.shot(), (x - 9, y - 9, x + 9, y + 9))

    def calibrate_idle_icon(self, timeout=12):
        """The reload button as it looks when nothing loads: the picture that stays
        the same for four looks in a row (1.6 s). Called on a local page."""
        t0, last, same = time.time(), None, 0
        while time.time() - t0 < timeout * self.slow:
            d = self.reload_icon()
            same = same + 1 if d == last else 1
            last = d
            if same >= 4:
                break
            time.sleep(0.4)
        self.idle_icon = last

    def wait_loaded(self, timeout=60):
        """Until the browser itself says the load is over: the reload button is the
        reload arrow again (as it was on the idle start page). Returns (seconds, done?)."""
        t0 = time.time()
        idle = getattr(self, "idle_icon", None)
        while idle is not None and time.time() - t0 < timeout * self.slow:
            if self.reload_icon() == idle:
                return time.time() - t0, True
            time.sleep(0.25)
        return time.time() - t0, idle is None

    def wait_ready(self, timeout=20):
        """Ready for a click on the page: the browser says the load is over (the reload button is
        the arrow again, no spinner) AND the content has not changed for 3 s; at most `timeout` s
        in all. (The missed link click of BUG-162 happened while euro-os.eu was still loading: 1.5 s
        of an unchanged picture is not enough.) Returns the seconds it took."""
        t0 = time.time()
        self.move(self.w - 4, self.h - 4)
        loaded, _ = self.wait_loaded(timeout)[0], None
        left = max(3.0, timeout - (time.time() - t0))
        self.settle(left, quiet=3.0)
        return time.time() - t0

    def close_infobars(self):
        """Close chrome's infobars. Only on the input test page: its background is not
        white, an infobar is, and nothing clickable sits under the close button. Each
        infobar pushes the page down by INFOBAR_H."""
        for _ in range(3):
            s = self.shot()
            top = s.px(*self.cx(6, CONTENT_TOP + 6))
            below = s.px(*self.cx(6, CONTENT_TOP + INFOBAR_H + 6))
            white = bytes((255, 255, 255))
            if not (top == white and below != white):
                self.infobars = 0
                break
            self.infobars = 1
            self.click(*self.cx(-30, CONTENT_TOP + 28))
            self.nap(1.2)
        return self.infobars

    def omnibox_selected(self, before, timeout=6):
        """True once the omnibox shows its selection after Ctrl+L: the URL text is highlighted
        (a run of selection blue in the omnibox row). Typing earlier sends the first letters to the
        page while the renderer is busy (BUG-145: 's://www.openstreetmap...')."""
        x0, y0 = self.cx(120, TOOLBAR_Y - 12)
        x1, y1 = self.cx(-130, TOOLBAR_Y + 12)
        t0 = time.time()
        while time.time() - t0 < timeout * self.slow:
            s = self.shot()
            run = best = 0
            for y in range(y0, y1, 3):
                row = s.rgb[(y * s.w + x0) * 3:(y * s.w + x1) * 3]
                run = 0
                for i in range(0, len(row) - 2, 3):
                    r, g, b = row[i], row[i + 1], row[i + 2]
                    if b >= 200 and r <= 120 and g <= 170 and b - r >= 100:
                        run += 1
                        best = max(best, run)
                    else:
                        run = 0
            if best >= 30:
                print(f"[chrome-smoke]   omnibox selection seen {time.time() - t0:.1f} s after Ctrl+L ({best} px run)", flush=True)
                return True
            time.sleep(0.2)
        print("[chrome-smoke]   omnibox selection NOT seen", flush=True)
        return False

    @staticmethod
    def url_is(u, host):
        """Does the tab's URL belong to `host`? By its host name, not by 'the text occurs somewhere'
        (a search page for a mangled URL contains the host name too)."""
        if u.startswith("file:") or "/" in host:        # "input-test", "euro-os.eu/en": a part of the address
            return host in u
        h = urllib.parse.urlparse(u).hostname or ""
        return u.startswith(("http://", "https://")) and (h == host or h.endswith("." + host) or h.endswith(host))

    def navigate(self, url, host, timeout=90, contains=None):
        """Ctrl+L, the URL, Enter. Returns (ok, seconds until the new document is there, detail).
        "There" is: the tab's URL has `host` in it AND its title is no longer the title
        of the page we left. (The URL alone changes the moment Enter is pressed, while
        the old document is still on the screen.)"""
        before = self.tabs() or []
        old_titles = {t for _, t in before}
        same = any(self.url_is(u, host) and (contains is None or contains in u) for u, _ in before)          # going to the page we are on
        for attempt in range(3):
            self.key("ctrl", "l")
            if self.omnibox_selected(None):
                break
            self.nap(1.0)                   # the browser is busy: ask again
        else:
            self.record_note = f"the omnibox never showed its selection after Ctrl+L ({host})"
        self.type(url)
        self.nap(0.2)
        t0 = time.time()
        self.key("ret")
        hit = self.wait_tab(lambda u, t: self.url_is(u, host) and (contains is None or contains in u) and t != "" and (same or t not in old_titles), timeout)
        if not hit:
            t = self.tabs()
            self.diag_stuck(f"navigation to {host} did not happen")
            return False, time.time() - t0, f"the tab never showed {host} with a title of its own: {t}"
        return True, hit[0], hit[1]

    # ── steps ───────────────────────────────────────────────────────────────
    def site_matrix(self, sites, tag=""):
        """Every site: typed in the omnibox, a screendump, a verdict."""
        results = []
        for site in sites:
            name = site["name"]
            self.new_bad()
            t_site = time.time()
            ok_nav, secs, detail = self.navigate(site["url"], site["host"])
            # The picture is judged when the BROWSER says the load is over (its reload
            # button is an arrow again), not when the tab has a URL and a title: the old
            # page stays on the screen for seconds after that.
            self.move(self.w - 4, self.h - 4)
            painted, loaded = self.wait_loaded(75)
            # A consent banner (YouTube's dialog, GitHub's bar, ...): answered before the
            # picture is judged, as a user would. Which button is in the report.
            if ok_nav and site.get("consent", True):
                c = self.handle_consent(f"{name}{tag}", expect=45 if site.get("consent_expected") else 0)
                self.consent_step(c)
                if c["passes"]:
                    self.move(self.w - 4, self.h - 4)
                    self.wait_loaded(30)
            s, paint, settled = self.settle(45, quiet=2.5, save_as=f"site-{name}{tag}")
            n, share = self.colours(s)
            bad = self.new_bad()
            # `loaded` is in the detail only: the reload-button comparison does not yet
            # recognise the idle arrow on every page, and then the picture is simply
            # judged after the full wait.
            stall = self.stall(f"site {name}{tag}", t_site)
            census = self.fds()
            ram = self.ramfiles()
            desk = self.deskstat()
            ok = ok_nav and n >= site["colours"] and not bad and stall < STALL_FAIL_MS
            why = f"{detail} after {secs:.1f} s, load {'finished' if loaded else 'NOT finished'} {painted:.1f} s later; content {n} colours, commonest {share:.0%}" + ("" if settled else ", still changing") \
                + f"; desktop loop: slowest console answer {stall:.0f} ms" \
                + (f"; fds: {census[0]} processes, {census[1]} descriptors, {census[2]} objects, {census[3]}/{census[4]} file objects" if census else "") \
                + (f"; RAM files: {ram[0]} files, {ram[1] // 1024} MiB of bytes, {ram[2] // 1024} MiB of holes" if ram else "") \
                + (f"; kernel stopwatch: longest gap between desktop passes {desk[0]} ms, {desk[1]} over 100 ms, console output queued at most {desk[2]} ms" if desk else "") \
                + (f"; {len(bad)} failure line(s): {bad[0]}" if bad else "")
            self.step(f"site {name}: {site['url']}", ok, why)
            results.append((name, ok, secs, n))
        return results

    def interaction_matrix(self, tag=""):
        """The interaction matrix: the input test page first (what was typed, copied,
        scrolled is read from its title), then the browser's own UI, the window, and
        a link with back and forward on the reference site."""
        rec = lambda name, ok, detail="": self.step(name + tag, ok, detail)
        short = tag.replace(" ", "").replace("(", "-").replace(")", "")
        # 1. omnibox to the local test page; typing in a form (US)
        ok, secs, detail = self.navigate("file:///tmp/input-test.html", "input-test")
        rec("omnibox: Ctrl+L, a URL, Enter", ok, f"{detail} after {secs:.1f} s")
        self.wait_state(lambda s: True)
        self.click(*self.page(*FIELD_A))
        text = "Hello EuroOS 2026, a/b?c=d&e-f_g:h;i.j"
        self.type(text)
        st = self.wait_state(lambda s: s[0] == text)
        rec("form: typed text arrives in the field (US)", bool(st) and st[0] == text, f"state {st}")
        # 2. select all, copy, paste in the other field
        self.key("ctrl", "a"); self.key("ctrl", "c")
        self.click(*self.page(*FIELD_B))
        self.key("ctrl", "v")
        st = self.wait_state(lambda s: s[1] == text and s[2] == "paste")
        rec("clipboard: Ctrl+A, Ctrl+C in field A, Ctrl+V in field B", bool(st) and st[1] == text and st[2] == "paste", f"state {st}")
        # 3. select text on the page with the mouse (double click = a word), copy, paste.
        #    Right after the previous paste: the second copy must replace the first.
        self.click(*self.page(*FIELD_B)); self.key("ctrl", "a"); self.key("delete")
        self.wait_state(lambda s: s[1] == "")
        self.click(*self.page(*WORD), double=True)
        self.shot("select-word" + short)
        self.key("ctrl", "c")
        self.click(*self.page(*FIELD_B))
        self.key("ctrl", "v")
        st = self.wait_state(lambda s: s[1].strip() != "")
        b = st[1] if st else ""
        rec("selection: double click selects a word on the page, copy, paste", b.strip() != "" and b.strip() in SENTENCE and b != text, f"field B = {b!r}")
        # 3b. a drag selects a stretch of text
        self.click(*self.page(*FIELD_B)); self.key("ctrl", "a"); self.key("delete")
        self.wait_state(lambda s: s[1] == "")
        x0, y0 = self.page(26, WORD[1]); x1, y1 = self.page(26 + 190, WORD[1])
        self.drag(x0, y0, x1, y1, steps=10)
        self.key("ctrl", "c")
        self.click(*self.page(*FIELD_B))
        self.key("ctrl", "v")
        st = self.wait_state(lambda s: len(s[1]) >= 8)
        b = st[1] if st else ""
        rec("selection: a drag with the button held selects text, copy, paste", len(b) >= 8 and b in SENTENCE, f"field B = {b!r}")
        # 4. scrolling: wheel, keys
        self.move(*self.page(400, 300))
        self.wheel(4)
        self.wait_state(lambda s: s[3] > 0); y_wheel = self.scroll_settled()
        self.click(*self.page(700, 300))            # focus on the page, not a field
        self.key("pgdn")
        self.wait_state(lambda s: s[3] > y_wheel); y_pg = self.scroll_settled()
        self.key("end")
        self.wait_state(lambda s: s[3] > y_pg); y_end = self.scroll_settled()
        self.key("home")
        self.wait_state(lambda s: s[3] == 0); y_home = self.scroll_settled()
        self.wheel(3)
        self.wait_state(lambda s: s[3] > 0); self.scroll_settled()
        self.wheel(-3)
        self.wait_state(lambda s: s[3] == 0); y_back = self.scroll_settled()
        rec("scroll: wheel down, PageDown, End, Home, wheel down and up again",
            0 < y_wheel < y_pg < y_end and y_home == 0 and y_back == 0,
            f"scrollY {y_wheel} -> {y_pg} -> {y_end} -> {y_home} -> {y_back}")
        # 5. reload: the fields are empty again
        self.click(*self.page(*FIELD_A)); self.key("end"); self.type("x")
        st = self.wait_state(lambda s: s[0] == text + "x")
        before = st[0] if st else None
        self.key("f5")
        hit = self.wait_tab(lambda u, t: "A=[] B=[]" in t, 20)
        rec("reload: F5 loads the page again", before == text + "x" and hit is not None, f"field A {before!r} -> title {self.title()!r}")
        # 6. AZERTY with AltGr: the layout is switched while the browser runs
        self.set_layout("be-azerty")
        self.click(*self.page(*FIELD_A))
        az = "azerty AZERTY 0123456789 m,;:=?./+ @#{}[]|€"
        self.type(az)
        st = self.wait_state(lambda s: s[0] == az)
        self.set_layout("us")
        rec("form: be-azerty with AltGr characters", bool(st) and st[0] == az, f"field A = {st[0] if st else None!r}")
        # 7. tabs: a second tab, a page in it, back to the first (typing lands in ITS
        #    field), close the second
        self.click(*self.page(*FIELD_B)); self.key("ctrl", "a"); self.key("delete")
        self.wait_state(lambda s: s[1] == "")
        self.key("ctrl", "t")
        two = self.wait_tab(lambda u, t: len(self.tabs() or []) == 2, 15)
        self.type("file:///tmp/euro.html"); self.key("ret")
        loaded = self.wait_tab(lambda u, t: "euro.html" in u and t != "", 30)
        self.settle(15, quiet=1.0, save_as="tab-two" + short)
        self.key("ctrl", "pgup")
        self.nap(0.8)
        self.click(*self.page(*FIELD_B)); self.type("tab one")
        st = self.wait_state(lambda s: s[1] == "tab one")
        first = bool(st) and st[1] == "tab one"
        self.key("ctrl", "pgdn"); self.nap(0.8)
        self.key("ctrl", "w")
        one = self.wait_tab(lambda u, t: len(self.tabs() or []) == 1 and "input-test" in u, 15)
        rec("tabs: Ctrl+T, a page in the new tab, Ctrl+PageUp to the first, Ctrl+PageDown, Ctrl+W",
            two is not None and loaded is not None and first and one is not None,
            f"two tabs {'ok' if two else 'no'}, second loaded {'ok' if loaded else 'no'}, typing in the first tab {'ok' if first else 'no'}, the first tab is left {'ok' if one else 'no'}")
        # 8. the browser's own popups: the menu opens at its button, Esc closes it
        closed = self.settle(10, quiet=0.8)[0]
        self.click(*self.cx(-23, TOOLBAR_Y))
        opened = self.settle(10, quiet=0.8, save_as="menu-open" + short)[0]
        mx0, my0 = self.cx(-340, TOOLBAR_Y + 20)
        mregion = (mx0, my0, mx0 + 330, my0 + 300)
        n, share = self.colours(opened, mregion)
        is_open = opened.differs(closed, min_pixels=20000, region=mregion)
        self.key("esc")
        again = self.settle(10, quiet=0.8)[0]
        rec("browser UI: the menu opens on a click at its button, Esc closes it",
            is_open and not again.differs(closed, min_pixels=3000, region=mregion), f"menu region: {n} colours while open")
        # 9. the window: move, maximise, restore
        self.window_steps(rec)
        # 10. a link, back, forward (the reference site's own navigation)
        ok, secs, detail = self.navigate("https://euro-os.eu/en/", "euro-os.eu/en")
        waited = self.wait_ready(20)
        self.click(*self.page(self.xw - 58 - 16, 33))        # the "Download" link in the page header
        hit = self.wait_tab(lambda u, t: u.endswith("#download"), 15)
        rec("link: a click on a link in the page navigates", ok and hit is not None, f"page ready after {waited:.1f} s; {self.tabs()}")
        self.wait_ready(20)
        self.key("alt", "left")
        back = self.wait_tab(lambda u, t: u.endswith("/en/"), 15)
        self.wait_ready(20)
        self.click(*self.cx(FORWARD_X, TOOLBAR_Y))
        fwd = self.wait_tab(lambda u, t: u.endswith("#download"), 15)
        rec("history: Alt+Left goes back, the toolbar's forward button goes forward", back is not None and fwd is not None,
            f"back {'ok' if back else 'no'}, forward {'ok' if fwd else 'no'}: {self.tabs()}")

    def window_steps(self, rec):
        wx, wy = self.win
        # drag by the title bar
        nx, ny = wx + 140, wy + 90
        wins = self.drag_window(self.win, wx + 300, wy + 22, nx + 300, ny + 22,
                                lambda w: abs(w[0] - nx) <= 12 and abs(w[1] - ny) <= 12)
        moved = next((w for w in wins if abs(w[0] - nx) <= 12 and abs(w[1] - ny) <= 12), None)
        if moved:
            self.win = moved
        self.click(*self.page(*FIELD_A)); self.key("ctrl", "a"); self.type("moved")
        st = self.wait_state(lambda s: s[0] == "moved")
        rec("window: drag by the title bar, input lands in the moved window", moved is not None and bool(st) and st[0] == "moved",
            f"frame at {self.win}, state {st}")
        # maximise with the green button
        pos = self.serial.mark()
        self.click(self.win[0] + 60, self.win[1] + 21)
        grown = self.serial.wait_for("resized to", 15 * self.slow, pos)
        self.track_size()
        big = (self.xw, self.xh)
        self.nap(1.0)
        old = self.win
        wins = self.look().windows()
        if wins:
            self.win = min(wins, key=lambda w: w[1])            # the maximised frame is the topmost one
        s, _, _ = self.settle(15, quiet=1.0, save_as="window-maximised")
        # chrome's toolbar reaches the right edge of the larger window: its menu button is there
        rx, ry = self.cx(-23, TOOLBAR_Y)
        dots = sum(1 for dy in range(-8, 9) for dx in range(-3, 4) if s.px(rx + dx, ry + dy) != s.px(rx - 14, ry - 14))
        self.click(*self.page(*FIELD_A)); self.key("ctrl", "a"); self.type("maximised")
        st = self.wait_state(lambda s: s[0] == "maximised")
        rec("window: maximise, chrome lays out for the new size, input lands",
            grown and big[0] > 1000 and dots >= 6 and bool(st) and st[0] == "maximised",
            f"X window {big[0]}x{big[1]}, menu button drawn at the new right edge: {dots >= 6}, state {st}")
        # restore with the same button
        pos = self.serial.mark()
        self.click(self.win[0] + 60, self.win[1] + 21)
        shrunk = self.serial.wait_for("resized to", 15 * self.slow, pos)
        self.track_size()
        self.nap(1.0)
        w = self.find_chrome([self.term])
        if w:
            self.win = w
        self.settle(15, quiet=1.0, save_as="window-restored")
        self.click(*self.page(*FIELD_B)); self.key("ctrl", "a"); self.type("restored")
        st = self.wait_state(lambda s: s[1] == "restored")
        rec("window: restore, input still lands", shrunk and (self.xw, self.xh) != big and bool(st) and st[1] == "restored",
            f"X window {self.xw}x{self.xh}, frame at {self.win}, state {st}")

    def perf(self, n_load=5, n_keys=30):
        """Page load of the reference site, and key-to-screen latency."""
        loads = []
        for i in range(n_load):
            self.navigate("file:///tmp/input-test.html", "input-test")
            self.settle(10, quiet=0.8)
            self.key("ctrl", "l"); self.nap(0.3)
            self.type("https://euro-os.eu/en/"); self.nap(0.3)
            region = self.content()
            before = self.digest(self.shot(), region)
            pos = self.serial.mark()
            t0 = time.time()
            self.key("ret", hold=0.02)
            # the first picture that is no longer the old page, then the final one
            first, seen, last, since = None, {}, None, t0
            while time.time() - t0 < 40:
                s = self.shot(); now = time.time()
                d = self.digest(s, region)
                if d != before and first is None:
                    first = now - t0
                seen.setdefault(d, now - t0)
                if d != last:
                    last, since = d, now
                if d != before and now - since >= 2.0:
                    break
            final = seen.get(last, 40.0)
            m = None
            for _ in range(20):
                if "loadEventFired" in self.serial.since(pos):
                    m = True
                    break
                time.sleep(0.1)
            loads.append((first if first is not None else 40.0, final))
            print(f"[chrome-smoke]   load {i + 1}: first change {loads[-1][0]:.2f} s, final picture {final:.2f} s", flush=True)
        firsts = sorted(l[0] for l in loads); finals = sorted(l[1] for l in loads)
        med_final = statistics.median(finals)
        self.step("perf: reference site loads in under 3 s (Enter to the final picture)", med_final < 3.0,
                  f"final picture median {med_final:.2f} s (min {finals[0]:.2f}, max {finals[-1]:.2f}), first change median {statistics.median(firsts):.2f} s, n={n_load}")
        # key to screen: a character typed in a form field, until the field's pixels change
        self.navigate("file:///tmp/input-test.html", "input-test")
        self.settle(10, quiet=0.8)
        self.click(*self.page(*FIELD_A))
        fx, fy = self.page(24, 85)
        region = (fx, fy, fx + 580, fy + 40)
        k = lambda q, down: {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": q}}}
        # Key to screen, two methods (11f-A). Each is 30 keys, with the kernel's
        # own `[keylat]` ledger reset before and printed after.
        #  - "screendump" (the method since W23): QMP screendump of the whole
        #    screen in a loop until the field's pixels change. A screendump holds
        #    QEMU's big lock for its whole copy of the screen (~15 ms); every
        #    guest exit that needs that lock (the UART, the xHCI registers the
        #    desktop loop polls, the HPET) waits for it, so the guest runs slower
        #    while it is measured. Reported with its capture time and bracket.
        #  - "fbread": QMP pmemsave of only the field's rows of the guest
        #    framebuffer (the GOP framebuffer, address and stride from the boot
        #    log), ~0.3 MB instead of a 6 MB screen: the lock is held for a
        #    fraction of a millisecond. The step's verdict uses this method.
        #  The field is emptied between the two: once the x's fill it, the text
        #  scrolls by one identical glyph and a key no longer changes its pixels
        #  (only the caret's next blink does, ~500 ms later).
        res, typed_first = {}, 0
        for method in ("screendump", "fbread"):
            if res:
                typed_first = len(self.field("A") or "")
                self.key("ctrl", "a")
                self.key("backspace")
                self.nap(0.6)
            res[method] = self._keys(n_keys, region, method)
        # --perf-smpuser-ab: the fbread keys once more with `smpuser on` (ring-3
        # threads may run on the APs, S5), then back off. Reported, not judged:
        # the evidence for or against moving more of chrome's work off the BSP.
        if getattr(self.a, "perf_smpuser_ab", False):
            self.key("ctrl", "a")
            self.key("backspace")
            self.nap(0.6)
            self.serial.send_line("smpuser on")
            self.nap(1.0)
            on = self._keys(n_keys, region, "fbread")
            self.serial.send_line("smpstat")
            self.nap(0.5)
            self.serial.send_line("smpuser off")
            self.nap(1.0)
            if on[0]:
                print(f"[chrome-smoke]   keys, fbread with smpuser on: median {statistics.median(on[0]):.0f} ms over {len(on[0])} keys "
                      f"(off: {statistics.median(res['fbread'][0]) if res['fbread'][0] else 0:.0f} ms)", flush=True)
            res["fbread-smpuser-on"] = on
        lat = res["fbread"][0] or res["screendump"][0]
        dump_ms = res["fbread"][1] or res["screendump"][1]
        old = res["screendump"][0]
        self.key_methods = {m: [round(x) for x in sorted(r[0])] for m, r in res.items()}
        n_keys *= 2
        typed = "x" * typed_first + (self.field("A") or "")
        if lat:
            lat.sort()
            med, p90 = statistics.median(lat), lat[int(len(lat) * 0.9) - 1]
            oldm = f"; screendump method (until 11e): median {statistics.median(old):.0f} ms" if old else ""
            self.step("perf: key to screen under 100 ms", med < 100 and len(typed) == n_keys,
                      f"median {med:.0f} ms, p90 {p90:.0f} ms, max {lat[-1]:.0f} ms over {len(lat)} keys ({len(typed)} characters arrived); "
                      f"one poll takes {statistics.median(dump_ms):.1f} ms, which is in every number{oldm}")
        else:
            self.step("perf: key to screen under 100 ms", False, "no key changed the field within 2 s")
        return loads, lat

    def _fb_geometry(self):
        """(base, stride in pixels) of the guest's GOP framebuffer, from the boot log."""
        log = self.serial.since(0)
        m = re.search(r"\[euro\] framebuffer @ (0x[0-9a-f]+)", log)
        g = re.search(r"\[euro\] GOP (\d+)x(\d+) stride=(\d+) (\w+)", log)
        if not m or not g or g.group(4) not in ("Bgr", "Rgb"):
            return None
        return int(m.group(1), 16), int(g.group(3))

    def fb_digest(self, region):
        """Digest of a screen region read straight from the guest framebuffer
        (QMP pmemsave of the region's rows; 4 bytes a pixel)."""
        base, stride = self.fb_geo
        x0, y0, x1, y1 = region
        f = os.path.join(self.wd, "fb.bin")
        r = self.qmp.cmd("pmemsave", val=base + y0 * stride * 4, size=(y1 - y0) * stride * 4, filename=f)
        self.last_dump_ret = time.time()
        if "error" in r:
            raise gs.Fatal(f"pmemsave: {r['error']}")
        raw = open(f, "rb").read()
        h = hashlib.sha1()
        for y in range(y1 - y0):
            h.update(raw[(y * stride + x0) * 4:(y * stride + x1) * 4])
        return h.digest()

    def _keys(self, n_keys, region, method):
        """n_keys presses of "x" into the focused field, each timed until the
        field's pixels change; returns (latencies ms, ms per poll)."""
        k = lambda q, down: {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": q}}}
        if method == "fbread":
            self.fb_geo = self._fb_geometry()
            if self.fb_geo is None:
                print("[chrome-smoke]   keys, fbread: no framebuffer address in the boot log; skipped", flush=True)
                return [], []
            look = lambda: self.fb_digest(region)
        else:
            look = lambda: self.digest(self.shot(), region)
        lat, dump_ms, cap, brk = [], [], [], []
        self.nap(1.2)                      # past the caret's first blink
        self.serial.send_line("keylat reset")
        self.nap(0.3)
        self.serial.send_line("sched-stat start")
        self.nap(0.3)
        for i in range(n_keys):
            before = look()
            t0 = time.time()
            self._send([k("x", True)])
            t_sent = time.time()
            dt, prev_ret, n = None, t_sent, 0
            while time.time() - t0 < 2.0:
                ts = time.time()
                d = look()
                tr = getattr(self, "last_dump_ret", None) or time.time()
                dump_ms.append((time.time() - ts) * 1000)
                n += 1
                if d != before:
                    dt = (time.time() - t0) * 1000
                    cap.append((tr - t0) * 1000)
                    brk.append((tr - prev_ret) * 1000)
                    break
                prev_ret = tr
            self._send([k("x", False)])
            if dt is not None:
                lat.append(dt)
                print(f"[chrome-smoke]   {method} key {i + 1}: {dt:.0f} ms (poll returned {cap[-1]:.0f} ms, bracket {brk[-1]:.1f} ms), "
                      f"QMP send {(t_sent - t0) * 1000:.1f} ms, {n} polls", flush=True)
            time.sleep(0.25)
        self.serial.send_line("sched-stat")
        self.nap(0.5)
        pos = self.serial.mark()
        self.serial.send_line("keylat")
        self.serial.wait_for("[keylat-cpu] delivery to screen", 10 * self.slow, pos)
        self.nap(0.5)
        if lat:
            print(f"[chrome-smoke]   keys, {method}: median {statistics.median(lat):.0f} ms, poll median {statistics.median(dump_ms):.1f} ms, "
                  f"bracket median {statistics.median(brk):.1f} ms, over {len(lat)} keys", flush=True)
        return lat, dump_ms

    def red_extent(self, s, rect):
        """Longest run of YouTube-red pixels in one row of `rect` (the player's progress bar:
        it grows while the video's time runs, and is visible even when the controls fade)."""
        x0, y0, x1, y1 = rect
        best = 0
        for y in range(y0, y1, 2):
            run = 0
            row = s.rgb[(y * s.w + x0) * 3:(y * s.w + x1) * 3]
            for i in range(0, len(row) - 2, 6):
                r, g, b = row[i], row[i + 1], row[i + 2]
                if r >= 220 and g <= 50 and b <= 90:
                    run += 2
                    best = max(best, run)
                else:
                    run = 0
        return best

    def play_and_measure(self, label, player, seconds, wav, title_probe=None, sample_every=4.0, bar=None):
        """The user's click on the middle of `player` (a screen rectangle) and then, for `seconds`:
        screendumps of the player (do frames change?), the page's own clock if it has one
        (title_probe() -> seconds or None), the progress bar (bar = rectangle) and the audio the
        guest sent to QEMU's wav file (RMS per 5 s). Returns a dict of what was measured."""
        self.click((player[0] + player[2]) // 2, (player[1] + player[3]) // 2)      # the user gesture
        self.nap(3.0)
        self.move(self.w - 4, self.h - 4)           # the pointer off the player: its controls fade
        wav0 = os.path.getsize(wav) if os.path.exists(wav) else 0
        t0 = time.time()
        digests, clock, bars, shots = [], [], [], 0
        clock0 = title_probe() if title_probe else None
        while time.time() - t0 < seconds:
            s = self.shot()
            digests.append(self.digest(s, player))
            if bar:
                bars.append(self.red_extent(s, bar))
            if title_probe:
                clock.append((time.time() - t0, title_probe()))
            if int(time.time() - t0) // 30 >= shots:
                shots += 1
                s.save_png(os.path.join(self.wd, f"{label}-play-{shots:02d}.png"))
            if self.new_bad():
                break
            time.sleep(sample_every)
        wall = time.time() - t0
        changes = sum(1 for i in range(1, len(digests)) if digests[i] != digests[i - 1])
        levels = wav_levels(wav, start=wav0)
        loud = sum(1 for v in levels if v > 40)
        res = {"samples": len(digests), "changed": changes, "wall_s": round(wall, 1), "audio_rms_5s": levels, "loud_windows": loud}
        if title_probe:
            vals = [v for _, v in clock if v is not None]
            res["page_clock_start"], res["page_clock_end"] = clock0, (vals[-1] if vals else None)
        if bar:
            res["progress_bar_px"] = (bars[0], bars[-1]) if bars else None
        return res

    def check_playback(self, label, res, seconds, clock=True, bar=False, picture=True, sound=True):
        """The verdicts of a video test: frames, time, sound."""
        n, ch = res["samples"], res["changed"]
        if picture:
            self.step(f"{label}: the picture advances for {seconds} s", n >= max(3, seconds // 6) and ch >= (n - 1) * 0.9,
                      f"{ch} of {n - 1} consecutive screendumps of the player differ")
        if clock:
            a, b = res.get("page_clock_start"), res.get("page_clock_end")
            ok = a is not None and b is not None and (b - a) >= 0.7 * res["wall_s"]
            self.step(f"{label}: the video's time increases", ok,
                      f"currentTime {a} -> {b} s in {res['wall_s']} s of wall clock")
        if bar:
            pb = res.get("progress_bar_px")
            self.step(f"{label}: the video's time increases (progress bar)", bool(pb) and pb[1] - pb[0] >= 4,
                      f"red progress bar {pb[0] if pb else None} -> {pb[1] if pb else None} px")
        lv = res["audio_rms_5s"]
        if sound:
          self.step(f"{label}: sound reaches the audio device for {seconds} s",
                    len(lv) >= seconds // 5 - 2 and res["loud_windows"] >= len(lv) * 0.8,
                    f"{res['loud_windows']} of {len(lv)} five-second windows of the guest's audio output (QEMU wav) above the noise floor (RMS {lv[:24]})")

    def youtube(self, seconds=120):
        """A YouTube video: picture that advances, the video's time, and sound, for `seconds`.
        The consent dialog is answered by clicking its button (reject if there is one); the play
        gesture is a real mouse click on the player. A 'confirm you're not a bot' page is
        reported as it is (screenshot) and the next public video is tried; it is not worked around."""
        rec = self.step
        ok, secs, detail = self.navigate("https://www.youtube.com/", "youtube.com")
        s, _, _ = self.settle(60, quiet=2.5, save_as="youtube-front")
        n, _ = self.colours(s)
        rec("youtube: the front page (or its consent dialog) loads", ok and n >= 30, f"{detail} after {secs:.1f} s, {n} colours")
        if not ok:
            return False
        self.consent_step(self.handle_consent("youtube-front", expect=45))
        blocked = []
        for k, url in enumerate(VIDEO_URLS):
            ok, secs, detail = self.navigate(url, "youtube.com", contains="watch?v=" + url.split("=")[-1])
            self.move(self.w - 4, self.h - 4)
            self.wait_loaded(60)
            s, _, _ = self.settle(60, quiet=3.0, save_as=f"youtube-watch-{k + 1}")
            rec(f"youtube: the watch page loads ({url.split('=')[-1]})", ok, f"{detail} after {secs:.1f} s")
            if not ok:
                continue
            self.consent_step(self.handle_consent(f"youtube-watch-{k + 1}"))
            bot, _ = self.find_in_page(["confirm you", "not a bot"], shot_name=f"youtube-botcheck-{k + 1}", button_only=False)
            if bot:
                # Guests need no login: a sign-in or bot page after the consent banner means the
                # banner was not handled (or the click did not register). Check that first, with
                # a screenshot of every step, before calling it YouTube's doing.
                again = self.handle_consent(f"youtube-watch-{k + 1}-recheck")
                self.consent_step(again)
                self.shot(f"youtube-watch-{k + 1}-after-recheck")
                bot, _ = self.find_in_page(["confirm you", "not a bot"], shot_name=f"youtube-botcheck-{k + 1}b", button_only=False)
            if bot:
                blocked.append(url)
                rec(f"youtube: {url.split('=')[-1]} is not behind a bot check", False,
                    "the page shows YouTube's 'Sign in to confirm you're not a bot' (find-in-page matched "
                    f"'{bot}'; screenshot youtube-botcheck-{k + 1}.png); reported as it is, not bypassed")
                continue
            break
        else:
            self.youtube_result = {"blocked_by_bot_check": blocked}
            return False
        self.youtube_result = {"video": url, "blocked_by_bot_check": blocked}
        # The player: top left of the page, 16:9 over most of the width at this window size.
        px0, py0 = self.page(24, 80)
        pw = min(self.xw - 48, 1280)
        player = (px0 + 20, py0 + 20, px0 + pw - 20, py0 + int(pw * 9 / 16) - 60)
        bar = (player[0], player[3] - 30, player[2], player[3] + 75)
        res = self.play_and_measure("youtube", player, seconds, os.path.join(self.wd, "audio.wav"), bar=bar)
        self.youtube_result.update(res)
        self.check_playback("youtube", res, seconds, clock=False, bar=True)
        return True

    def consent_selftest(self, port):
        """The consent handling against three local banners (reject + accept, Dutch labels,
        accept only): the page's title says which button it got."""
        for page, want in (("consent-en.html", "rejected"), ("consent-nl.html", "rejected"), ("consent-accept.html", "accepted")):
            ok, secs, detail = self.navigate(f"http://10.0.2.2:{port}/{page}", "10.0.2.2")
            self.settle(15, quiet=1.0)
            if not ok:
                self.step(f"consent self-test {page}: the page loads", False, detail)
                continue
            c = self.handle_consent(page)
            self.consent_step(c)
            self.step(f"consent self-test {page}: the page was told '{want}'", want in self.title(), f"title {self.title()!r}")

    def localvideo(self, port, seconds):
        """H.264/AAC and VP9/Opus clips from this host (http://10.0.2.2:PORT/), with the same
        checks as YouTube: frames change, the video's currentTime runs (the page mirrors it into
        its title), and the guest's audio output is not silence."""
        wav = os.path.join(self.wd, "audio.wav")
        self.media_dir = LocalHandler.media_dir
        results = {}
        clips = (("h264+aac", "h264.html", True, True), ("vp9+opus", "vp9.html", True, True),
                 ("vp9 video only", "vp9v.html", True, False), ("opus audio only", "opus.html", False, True))
        for codec, page, want_pic, want_snd in clips:
            if not os.path.isfile(os.path.join(self.media_dir, page.replace(".html", ".mp4" if "h264" in page else ".webm"))):
                continue
            label = f"local video {codec}"
            ok, secs, detail = self.navigate(f"http://10.0.2.2:{port}/{page}", "10.0.2.2")
            self.settle(15, quiet=1.0, save_as=f"local-{page}-loaded")
            hit = self.wait_tab(lambda u, t: t.startswith("V=["), 20)
            st = self.video_state()
            self.step(f"{label}: the page loads from the host and the browser knows the codec", ok and st is not None and st["can"] != "no",
                      f"{detail}; state {st}")
            if not (ok and st):
                continue
            px0, py0 = self.page(8, 8)
            player = (px0, py0, px0 + 480, py0 + 270)
            btn = self.page(150, 340)
            # the play button is a real click; measure_and_play clicks the middle of `player`, so the
            # button click is done here and the player click is skipped by a one-point "player".
            self.click(*btn)
            self.nap(3.0)
            self.move(self.w - 4, self.h - 4)
            wav0 = os.path.getsize(wav) if os.path.exists(wav) else 0
            t0 = time.time()
            digests, clock = [], []
            while time.time() - t0 < seconds:
                sh = self.shot()
                digests.append(self.digest(sh, player))
                v = self.video_state()
                clock.append((time.time() - t0, v))
                time.sleep(2.0)
            wall = time.time() - t0
            levels = wav_levels(wav, start=wav0)
            loud = sum(1 for x in levels if x > 40)
            times = [v["t"] for _, v in clock if v]
            changes = sum(1 for i in range(1, len(digests)) if digests[i] != digests[i - 1])
            res = {"samples": len(digests), "changed": changes, "wall_s": round(wall, 1), "audio_rms_5s": levels,
                   "loud_windows": loud, "page_clock_start": times[0] if times else None,
                   "page_clock_end": times[-1] if times else None, "last_state": clock[-1][1] if clock else None}
            results[codec] = res
            self.check_playback(label, res, seconds, picture=want_pic, sound=want_snd)
            err = (clock[-1][1] or {}).get("err") if clock else None
            self.step(f"{label}: no media error", err == "none", f"last state {res['last_state']}")
        self.localvideo_result = results

    def video_state(self):
        """The local video page's title, V=[st=playing t=1.50 rs=4 err=none can=probably]: as a dict."""
        for url, title in self.tabs() or []:
            m = re.match(r"V=\[st=(\S+) t=([\d.]+) rs=(\d) err=(\S+) can=(\S*)\]", title)
            if m:
                return {"st": m.group(1), "t": float(m.group(2)), "rs": int(m.group(3)), "err": m.group(4), "can": m.group(5)}
        return None

    # ── consent banners (generic) ───────────────────────────────────────────
    def highlight(self, after, before, region):
        """(centre, bbox) of the find-in-page active-match highlight (orange pixels that `before`
        did not have), or None when there are fewer than 40 of them."""
        x0, y0, x1, y1 = region
        xs, ys = [], []
        for y in range(y0, y1):
            row = after.rgb[(y * after.w + x0) * 3:(y * after.w + x1) * 3]
            i = row.find(FIND_ACTIVE)
            while i >= 0:
                if i % 3 == 0 and before.px(x0 + i // 3, y) != FIND_ACTIVE:
                    xs.append(x0 + i // 3)
                    ys.append(y)
                i = row.find(FIND_ACTIVE, i + 1)
        if len(xs) < 40:
            return None
        bbox = (min(xs), min(ys), max(xs), max(ys))
        xs.sort(); ys.sort()
        return (xs[len(xs) // 2], ys[len(ys) // 2]), bbox

    def isolated(self, after, bbox, pad=8):
        """Is the highlighted text the whole content of its line segment (a button label), not a
        word inside a sentence? The `pad` columns left and right of the highlight, over its height
        (minus the glyph margins), must be one flat colour. 'Reject' inside 'rejected', or
        'Accept all' inside 'If you choose to \u201cAccept all,\u201d', fails this."""
        x0, y0, x1, y1 = bbox
        for xa, xb in ((x0 - pad, x0 - 1), (x1 + 1, x1 + pad)):
            px = [after.px(x, y) for y in range(y0 + 2, max(y0 + 3, y1 - 1)) for x in range(max(0, xa), min(after.w, xb + 1))]
            if not px:
                return False
            # "flat": every channel within 24 of the median pixel (buttons are gradients or dithered)
            ref = sorted(px, key=lambda c: c[0] + c[1] + c[2])[len(px) // 2]
            flat = sum(1 for c in px if max(abs(c[0] - ref[0]), abs(c[1] - ref[1]), abs(c[2] - ref[2])) <= 24)
            if flat < 0.97 * len(px):
                return False
        return True

    def stable_highlight(self, base, region, shot_name=None):
        """The highlight once it has stopped moving: the find bar paints (and scrolls) a second or so
        after the key in this guest, so "nothing" needs three looks 0.5 s apart and a position two
        looks that agree (at most 8 looks). (screen, (centre, bbox) or None)."""
        prev, nones, s, hit = None, 0, None, None
        for k in range(8):
            self.nap(0.5)
            s = self.shot(shot_name) if (shot_name and k == 0) else self.shot()
            hit = self.highlight(s, base, region)
            if hit is None:
                nones += 1
                if nones >= 3:
                    break
                prev = None
                continue
            nones = 0
            if prev is not None and abs(hit[0][0] - prev[0]) < 2 and abs(hit[0][1] - prev[1]) < 2:
                break
            prev = hit[0]
        return s, hit

    def find_in_page(self, labels, shot_name=None, button_only=True, max_matches=8):
        """Ctrl+F each label in turn, and Enter through its matches (find scrolls each into view).
        (label, (x, y)) of the first match that is highlighted and, with button_only, stands
        alone like a button label (isolated()); else (None, None). The find bar is closed."""
        region = self.content()
        base = self.shot()
        self.key("ctrl", "f")
        self.nap(1.5)                           # the find bar must have focus before the first letter (it ate letters of "reject all")
        self.type("zq")                         # warm-up: the first letters typed into a fresh find bar are sometimes lost
        self.nap(0.8)
        found = (None, None)
        self.find_trace = []                    # per label: [(highlight centre, passed the button test)]
        for label in labels:
            self.key("ctrl", "a")
            self.type(label)
            self.nap(0.5)
            repeats, last = 0, None
            for m in range(max_matches):
                s, hit = self.stable_highlight(base, region, shot_name if (shot_name and m == 0) else None)
                if not hit and m == 0 and label in CONSENT_RETRY:
                    # A full reject label that finds nothing: is the text in the find bar what was
                    # typed? Keep the picture of the find bar, then type it again, slowly.
                    self.find_misses += 1
                    s.save_png(os.path.join(self.wd, f"findbar-miss-{self.find_misses:02d}-{label.replace(' ', '_')}.png"))
                    self.key("ctrl", "a")
                    for ch in label:
                        self.type(ch)
                        self.nap(0.15)
                    self.nap(0.5)
                    s, hit = self.stable_highlight(base, region)
                    self.find_retries.append((label, bool(hit)))
                if not hit:
                    break
                centre, bbox = hit
                ok_btn = self.isolated(s, bbox)
                self.find_trace.append((label, m, centre, ok_btn))
                if not button_only or ok_btn:
                    found = (label, centre)
                    break
                if m and centre == last:
                    repeats += 1                # the highlight did not move: the scroll is still under way, or wrapped round
                    if repeats >= 2:
                        break
                last = centre
                self.key("ret")                 # the next match
                self.nap(0.5)
            if found[0]:
                break
        self.key("esc")
        self.nap(0.4)
        return found

    def handle_consent(self, name, passes=2, expect=0):
        """Answer a cookie/consent banner like a person: find 'Reject all' (or its Dutch, French,
        German form), otherwise 'Accept all' (or its forms), click it with the tablet, and check
        that the banner is gone (the label is no longer on the page and the pixels around the
        button changed). Returns {site, passes: [{button, kind, at, gone}], ok}; ok is False
        only when a button was clicked and the banner stayed (an input bug until shown otherwise)."""
        out = {"site": name, "passes": [], "ok": True}
        tag = re.sub(r"[^a-z0-9]+", "-", name.lower())
        for n in range(passes):
            pre = self.shot()
            label, pos = self.find_in_page(CONSENT_REJECT + CONSENT_ACCEPT)
            trace = list(self.find_trace)
            # A banner that is known to come (expect = seconds to wait): the dialog is drawn some
            # seconds after the load event, and a click before it is there clicks nothing.
            t_end = time.time() + (expect if n == 0 else 0)
            while not pos and time.time() < t_end:
                self.nap(3.0)
                pre = self.shot()
                label, pos = self.find_in_page(CONSENT_REJECT + CONSENT_ACCEPT)
                trace = list(self.find_trace)
            if not pos:
                break
            kind = "reject" if label in CONSENT_REJECT else "accept"
            pre = self.shot(f"consent-{tag}-{n + 1}-before")      # the button, find bar closed
            self.click(*pos)
            self.nap(2.5)
            self.move(self.w - 4, self.h - 4)
            self.wait_loaded(20)
            self.settle(10, quiet=1.0)
            post = self.shot(f"consent-{tag}-{n + 1}-after")
            around = (max(0, pos[0] - 70), max(0, pos[1] - 25), min(self.w, pos[0] + 70), min(self.h, pos[1] + 25))
            changed = post.differs(pre, min_pixels=150, region=around)
            again, _ = self.find_in_page([label])
            gone = again is None and changed
            out["passes"].append({"button": label, "kind": kind, "at": list(pos), "gone": gone,
                                  "label_still_found": again is not None, "pixels_changed": changed})
            out["passes"][-1]["find_trace"] = [t for t in trace if t[0] in CONSENT_REJECT]
            self.consent_log.append(dict(out["passes"][-1], site=name))
            if not gone:
                out["ok"] = False
                break
        return out

    def consent_step(self, c):
        """One step per site: which button was clicked, and was the banner gone."""
        if not c["passes"]:
            self.step(f"consent {c['site']}: no banner button found", True,
                      f"find-in-page matched none of {len(CONSENT_REJECT) + len(CONSENT_ACCEPT)} labels")
            return
        what = "; ".join(f"clicked '{p['button']}' ({p['kind']}) at {tuple(p['at'])}, banner "
                         + (f" [matches of the reject labels, (label, n, at, looked like a button): {p['find_trace']}]" if p["kind"] == "accept" and p.get("find_trace") else "")
                         + ("gone" if p["gone"] else f"STILL THERE (label found again: {p['label_still_found']}, pixels around the button changed: {p['pixels_changed']})")
                         for p in c["passes"])
        self.step(f"consent {c['site']}: {c['passes'][0]['kind']} button clicked", c["ok"],
                  what + (f"; full reject labels that found nothing at first and what typing them again slowly did: {self.find_retries}" if self.find_retries else ""))

    def memory(self):
        """The kernel's own memory account while a browser runs: the newest `[cpu]`
        heartbeat line (every 30 s): kernel heap, big-block pool, RAM files, demand pool."""
        lines = re.findall(r"\[cpu\] \d+ ticks since last heartbeat; (heap \d+ MiB used, \d+ MiB free; big \d+/\d+ MiB \(peak \d+\); files \d+ MiB; demand \d+ MiB free)", self.serial.since(0))
        return lines[-1] if lines else "no [cpu] heartbeat line yet"


def wav_levels(path, window=5.0, start=0):
    """RMS per `window` seconds of a 16-bit WAV that QEMU is still writing, from byte
    `start` of the file on (what was there before the measurement is not counted)."""
    out = []
    try:
        with open(path, "rb") as f:
            data = f.read()
        if len(data) < 64:
            return out
        ch, rate = struct.unpack_from("<HI", data, 22)
        frame0 = 2 * ch
        begin = max(44, start - (start - 44) % frame0)
        pcm = memoryview(data)[begin:]
        pcm = pcm[:len(pcm) - len(pcm) % frame0]
        frame = 2 * ch
        n = int(rate * window) * frame
        for off in range(0, len(pcm) - n + 1, n):
            chunk = pcm[off:off + n].cast("h")
            step = max(1, len(chunk) // 20000)
            acc = sum(v * v for v in chunk[::step])
            out.append(int((acc / max(1, len(chunk[::step]))) ** 0.5))
    except (OSError, struct.error):
        pass
    return out


# ── the host-side web server of the local tests ──────────────────────────────────────────────
VIDEO_PAGE = """<!doctype html><meta charset=utf-8><title>V=[st=loading t=0.00 rs=0 err=none can=?]</title>
<body style="margin:8px;background:#fff;font:20px sans-serif">
<video id=v width=480 height=270 src="%(file)s" preload=auto style="display:block;background:#000"></video>
<button id=b style="margin-top:30px;width:300px;height:80px;font-size:32px">PLAY</button>
<script>
var v = document.getElementById('v'), can = v.canPlayType('%(type)s') || 'no';
function upd(err) { document.title = 'V=[st=' + (v.ended ? 'ended' : v.paused ? 'paused' : 'playing') + ' t=' + v.currentTime.toFixed(2) +
  ' rs=' + v.readyState + ' err=' + (err || (v.error ? v.error.code : 'none')) + ' can=' + can + ']'; }
setInterval(upd, 500); upd();
document.getElementById('b').onclick = function () { v.play().catch(function (e) { upd(e.name); }); };
</script>"""
CONSENT_PAGE = """<!doctype html><meta charset=utf-8><title>consent=open</title>
<body style="margin:0;background:#eef;font:18px sans-serif"><h1 style="margin:40px">A page with a banner</h1>
<p style="margin:40px">Some text. Some more text.</p>
<div id=bn style="position:fixed;left:0;right:0;bottom:0;height:150px;background:#fff;border-top:4px solid #036;padding:16px">
We use cookies to make this page work.<br><br>%(buttons)s</div>
<script>function done(w) { document.getElementById('bn').style.display = 'none'; document.title = 'consent=' + w; }</script>"""
LOCAL_PAGES = {
    "h264.html": VIDEO_PAGE % {"file": "h264.mp4", "type": 'video/mp4; codecs="avc1.42E01E, mp4a.40.2"'},
    "vp9.html": VIDEO_PAGE % {"file": "vp9.webm", "type": 'video/webm; codecs="vp9, opus"'},
    # the same clip split in its two halves: which one breaks?
    "vp9v.html": VIDEO_PAGE % {"file": "vp9v.webm", "type": 'video/webm; codecs="vp9"'},
    "opus.html": VIDEO_PAGE % {"file": "opus.webm", "type": 'audio/webm; codecs="opus"'},
    "consent-en.html": CONSENT_PAGE % {"buttons": "<button onclick=\"done('accepted')\" style='font-size:20px;padding:10px'>Accept all</button> "
                                                  "<button onclick=\"done('rejected')\" style='font-size:20px;padding:10px'>Reject all</button>"},
    "consent-nl.html": CONSENT_PAGE % {"buttons": "<button onclick=\"done('accepted')\" style='font-size:20px;padding:10px'>Alles accepteren</button> "
                                                  "<button onclick=\"done('rejected')\" style='font-size:20px;padding:10px'>Alles weigeren</button>"},
    "consent-accept.html": CONSENT_PAGE % {"buttons": "<button onclick=\"done('accepted')\" style='font-size:20px;padding:10px'>I agree</button>"},
}


class LocalHandler(http.server.BaseHTTPRequestHandler):
    """The test pages and the clips of --media-dir, with Range requests (the media stack seeks)."""
    media_dir = "."
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def do_GET(self):
        name = os.path.basename(self.path.split("?")[0])
        if name in LOCAL_PAGES:
            body, ctype, status, hdr = LOCAL_PAGES[name].encode(), "text/html; charset=utf-8", 200, {}
        else:
            path = os.path.join(self.media_dir, name)
            if name not in ("h264.mp4", "vp9.webm", "vp9v.webm", "opus.webm") or not os.path.isfile(path):
                self.send_error(404)
                return
            size = os.path.getsize(path)
            ctype = "video/mp4" if name.endswith(".mp4") else "video/webm"
            status, a, b = 200, 0, size - 1
            m = re.match(r"bytes=(\d*)-(\d*)", self.headers.get("Range", ""))
            if m and (m.group(1) or m.group(2)):
                if m.group(1):
                    a = int(m.group(1))
                    b = min(int(m.group(2)), size - 1) if m.group(2) else size - 1
                else:
                    a = max(0, size - int(m.group(2)))
                status = 206
            with open(path, "rb") as f:
                f.seek(a)
                body = f.read(b - a + 1)
            hdr = {"Content-Range": f"bytes {a}-{b}/{size}"} if status == 206 else {}
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Cache-Control", "no-store")
        for k, v in hdr.items():
            self.send_header(k, v)
        self.end_headers()
        try:
            self.wfile.write(body)
        except OSError:
            pass


def serve_local(port, media_dir):
    """Serve the local test pages on 127.0.0.1:PORT of THIS host. The guest reaches it at
    10.0.2.2:PORT (QEMU user-net's host address); nothing is forwarded to the outside."""
    LocalHandler.media_dir = media_dir
    class Srv(socketserver.ThreadingMixIn, http.server.HTTPServer):
        daemon_threads = True
    srv = Srv(("127.0.0.1", port), LocalHandler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def make_args(a, workdir):
    extra = (f"-drive format=raw,file={a.pack},if=virtio,readonly=on -drive format=raw,file={a.nss},if=virtio,readonly=on "
             f"-netdev user,id=n0 -device {a.nic},netdev=n0 "
             f"-audiodev wav,id=a0,path={os.path.join(workdir, 'audio.wav')} -device intel-hda -device hda-duplex,audiodev=a0 " + a.extra)
    return types.SimpleNamespace(image=a.image, workdir=workdir, kvm=a.kvm, cpu_opts="", smp=a.smp, mem=a.mem, pin=a.pin,
                                 input=a.input, net="none", layout="auto", extra=extra,
                                 boot_timeout=300 if a.kvm else 1500, alive_timeout=40 if a.kvm else 240,
                                 slow=1.0 if a.kvm else 3.0, soak=0, seed=0, skip_smoke=True, keep_image=False)


def one_boot(a, workdir, body):
    """Boot, run `body(g)`, stop. Returns (guest, verdict, rc)."""
    os.makedirs(workdir, exist_ok=True)
    if os.path.exists(os.path.join(workdir, "qmp.sock")):
        sys.exit(f"{workdir} has a live qmp.sock: another run uses this work directory")
    g = Chrome(make_args(a, workdir))
    verdict, rc = "PASS", 0
    try:
        g.boot()
        g.alive("boot")
        body(g)
        g.alive("end")
    except gs.Fatal as e:
        verdict, rc = f"CRASH/HANG: {e}", 2
    except KeyboardInterrupt:
        verdict, rc = "interrupted", 1
    finally:
        if g.probe is not None:
            g.probe.halt = True
        g.stop()
    failed = [r for r in g.results if not r[1]]
    if rc == 0 and failed:
        verdict, rc = f"FAIL ({len(failed)} step(s))", 1
    return g, verdict, rc


def write_report(a, workdir, g, verdict, t0, extra=None):
    probe = getattr(g, "probe", None)
    worst, answers = probe.worst() if probe else (0.0, 0)
    report = {"verdict": verdict, "image": os.path.abspath(a.image), "accel": "kvm" if a.kvm else "tcg", "smp": a.smp,
              "mem": a.mem, "input": a.input, "nic": a.nic, "seconds": int(time.time() - t0),
              "max_console_latency_ms": g.max_echo_ms,
              "desktop_stall_ms": {"slowest_console_answer": round(worst), "median": round(probe.median()) if probe else 0,
                                   "answers": answers, "per_step": getattr(g, "stalls", []),
                                   "nmi_sent_after_s": probe.nmis if probe else []},
              "steps": [{"name": n, "ok": ok, "detail": d} for n, ok, d in g.results]}
    report.update(extra or {})
    with open(os.path.join(workdir, "report.json"), "w") as f:
        json.dump(report, f, indent=1)
    with open(os.path.join(workdir, "report.txt"), "w") as f:
        f.write(f"chrome-smoke: {verdict}\n{report['accel']} smp={a.smp} mem={a.mem} input={a.input} nic={a.nic}, {report['seconds']} s, "
                f"max console latency {g.max_echo_ms} ms; desktop loop while the browser ran: slowest console answer "
                f"{round(worst)} ms, median {round(probe.median()) if probe else 0} ms over {answers} echoes\n\n")
        for n, ok, d in g.results:
            f.write(f"{'ok  ' if ok else 'FAIL'} {n}{': ' + d if d else ''}\n")
        for k, v in (extra or {}).items():
            f.write(f"\n{k}: {json.dumps(v, indent=1) if not isinstance(v, str) else v}\n")
    ok = sum(1 for r in g.results if r[1])
    print(f"[chrome-smoke] {verdict} - {ok}/{len(g.results)} steps ok, report and screenshots in {workdir}", flush=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--image", default="eurokernel.img")
    ap.add_argument("--pack", required=True, help="the Chromium EuroPack disk (chrome-pack2.img)")
    ap.add_argument("--nss", required=True, help="the NSS EuroPack disk; its NSS generation must match the chrome pack's (scripts/mk-nss-pack.sh)")
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--kvm", action="store_true")
    ap.add_argument("--smp", type=int, default=2)
    ap.add_argument("--mem", default="6144M")
    ap.add_argument("--pin", default="")
    ap.add_argument("--input", choices=["usb", "virtio"], default="usb")
    ap.add_argument("--nic", default="virtio-net-pci", help="virtio-net-pci or e1000e (the userspace driver e1000d)")
    ap.add_argument("--extra", default="")
    ap.add_argument("--only", default="", help="comma list of: sites, interact, youtube, perf (default: sites, interact, perf)")
    ap.add_argument("--perf-smpuser-ab", action="store_true", help="perf: type the fbread keys once more with smpuser on (S5), reported only")
    ap.add_argument("--sites", default="", help="comma list of site names (default: all of the matrix)")
    ap.add_argument("--rounds", type=int, default=1, help="repeat the site matrix this many times in one browser session")
    ap.add_argument("--soak-hours", type=float, default=0.0, help="repeat sites + interaction in one session for this long")
    ap.add_argument("--starts", type=int, default=0, help="N fresh boots: start chrome, close the infobar, type a URL in the omnibox")
    ap.add_argument("--video-seconds", type=int, default=120)
    ap.add_argument("--local-seconds", type=int, default=60, help="how long each local clip plays (--only localvideo)")
    ap.add_argument("--local-port", type=int, default=18780, help="port of the host-side test server (127.0.0.1; the guest uses 10.0.2.2:PORT)")
    ap.add_argument("--media-dir", default="", help="directory with h264.mp4 and vp9.webm (scripts/mk-test-video.sh)")
    a = ap.parse_args()
    if gs.OVMF is None:
        sys.exit("no OVMF firmware found")
    a.workdir = os.path.abspath(a.workdir)
    os.makedirs(a.workdir, exist_ok=True)
    only = [x for x in a.only.split(",") if x] or ["sites", "interact", "perf"]
    sites = [s for s in SITES if not a.sites or s["name"] in a.sites.split(",")]
    t0 = time.time()

    if a.starts:
        rows = []
        for i in range(a.starts):
            wd = os.path.join(a.workdir, f"start-{i + 1:02d}")
            def body(g):
                if not g.start_chrome("file:///tmp/input-test.html"):
                    return
                g.close_infobars()
                g.move(g.w - 4, g.h - 4)
                g.calibrate_idle_icon()
                g.click(*g.page(*FIELD_A)); g.type("first-key")
                # The page puts the field into its title, which reaches the console
                # a moment after the last key: wait for it, do not sample once.
                st = g.wait_state(lambda st: st[0] == "first-key", timeout=6)
                v = st[0] if st else None
                g.step("page input right after start: typed text arrives", v == "first-key", f"field A = {v!r}")
                t = time.time()
                ok, secs, detail = g.navigate("https://euro-os.eu/", "euro-os.eu")
                # Judged when the browser says the load is over (as in the site
                # matrix), not after the first 1.5 s without change: the colour
                # count of a half-painted page fell under the threshold.
                g.move(g.w - 4, g.h - 4)
                g.wait_loaded(60)
                s, _, _ = g.settle(30, quiet=2.5, save_as="start-site")
                n, _ = g.colours(s)
                g.step("omnibox right after start: Ctrl+L, URL, Enter", ok and n >= 400, f"{detail} after {secs:.1f} s, {n} colours")
                r = g.shot()
                g.click(*g.cx(-23, TOOLBAR_Y)); g.nap(1.0)
                m = g.shot("start-menu")
                opened = m.differs(r, min_pixels=8000)
                g.key("esc"); g.nap(0.5)
                g.step("browser UI right after start: the menu opens on a click", opened)
            g, verdict, rc = one_boot(a, wd, body)
            write_report(a, wd, g, verdict, t0)
            rows.append((i + 1, verdict, [r[1] for r in g.results]))
            print(f"[chrome-smoke] start {i + 1}/{a.starts}: {verdict}", flush=True)
        good = sum(1 for r in rows if r[1] == "PASS")
        with open(os.path.join(a.workdir, "starts.txt"), "w") as f:
            f.write(f"chrome-smoke --starts {a.starts}: {good}/{a.starts} PASS\n")
            for i, v, steps in rows:
                f.write(f"start {i:02d}: {v} {''.join('.' if s else 'F' for s in steps)}\n")
        print(f"[chrome-smoke] starts: {good}/{a.starts} PASS", flush=True)
        sys.exit(0 if good == a.starts else 1)

    extra = {}
    if "localvideo" in only or "consent" in only:
        if "localvideo" in only and not (os.path.isfile(os.path.join(a.media_dir, "h264.mp4")) and os.path.isfile(os.path.join(a.media_dir, "vp9.webm"))):
            sys.exit("--only localvideo needs --media-dir with h264.mp4 and vp9.webm (scripts/mk-test-video.sh DIR)")
        serve_local(a.local_port, a.media_dir)

    def body(g):
        if not g.start_chrome("file:///tmp/input-test.html"):
            return
        g.close_infobars()
        g.move(g.w - 4, g.h - 4)
        g.calibrate_idle_icon()
        g.step("chrome's infobars are closed or absent", g.infobars == 0, f"{g.infobars} left")
        extra["memory_start"] = g.memory()
        if a.soak_hours > 0:
            end = time.time() + a.soak_hours * 3600
            rnd, mem = 0, []
            while time.time() < end:
                rnd += 1
                g.site_matrix(sites, tag=f"-r{rnd}" if rnd <= 3 else "-last")
                g.interaction_matrix(tag=f" (round {rnd})")
                mem.append((int(time.time() - t0), g.memory()))
                print(f"[chrome-smoke] soak round {rnd} done after {int(time.time() - t0)} s: {mem[-1][1]}", flush=True)
                extra["soak_rounds"], extra["soak_memory"] = rnd, mem
                write_report(a, a.workdir, g, "running", t0, extra)
            return
        if "sites" in only:
            for r in range(a.rounds):
                g.site_matrix(sites, tag=f"-r{r + 1}" if a.rounds > 1 else "")
        if "interact" in only:
            g.interaction_matrix()
        if "consent" in only:
            g.consent_selftest(a.local_port)
        if "localvideo" in only:
            g.localvideo(a.local_port, a.local_seconds)
            extra["localvideo"] = getattr(g, "localvideo_result", None)
        if "youtube" in only:
            g.youtube(a.video_seconds)
            extra["youtube"] = getattr(g, "youtube_result", None)
        if "perf" in only:
            loads, lat = g.perf()
            extra["perf_loads_s"] = loads
            extra["perf_key_ms"] = [round(x) for x in lat]
            extra["perf_key_ms_by_method"] = getattr(g, "key_methods", {})
        extra["consent_clicks"] = g.consent_log
        extra["memory_end"] = g.memory()

    g, verdict, rc = one_boot(a, a.workdir, body)
    write_report(a, a.workdir, g, verdict, t0, extra)
    sys.exit(rc)


if __name__ == "__main__":
    main()
