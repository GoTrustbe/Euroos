#!/usr/bin/env python3
"""EuroOS live-try orchestrator.

Gives a visitor a throw-away, browser-accessible VNC session of EuroOS for a
limited time. Flow:

  browser  --POST /api/launch {email}-->  this orchestrator
     |                                        |
     |                              start a wall-off QEMU (qcow2 overlay,
     |                              no guest network, seccomp sandbox) on a
     |                              private VNC port; write a websockify
     |                              token file; log the e-mail + IP; arm a
     |                              30-minute reaper.
     |
     <--- { url: /vnc/<token> } ---
     |
  browser opens /vnc/<token>  ->  nginx -> noVNC -> websockify(token) -> QEMU VNC

Everything binds to 127.0.0.1; nginx is the only public front. No third-party
Python packages — standard library only (sovereign, auditable).
"""

import json
import os
import re
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# ----------------------------------------------------------------------------
# Configuration (override via environment in the systemd unit)
# ----------------------------------------------------------------------------
LISTEN_HOST      = os.environ.get("EUROVNC_HOST", "127.0.0.1")
LISTEN_PORT      = int(os.environ.get("EUROVNC_PORT", "6070"))
MAX_SESSIONS     = int(os.environ.get("EUROVNC_MAX_SESSIONS", "3"))
SESSION_TTL      = int(os.environ.get("EUROVNC_TTL", "1800"))          # 30 min
IP_COOLDOWN      = int(os.environ.get("EUROVNC_IP_COOLDOWN", "120"))   # s between launches per IP
VNC_DISPLAY_MIN  = int(os.environ.get("EUROVNC_DISPLAY_MIN", "1"))     # TCP 5901
VNC_DISPLAY_MAX  = int(os.environ.get("EUROVNC_DISPLAY_MAX", "9"))     # TCP 5909
BASE_IMAGE       = os.environ.get("EUROVNC_BASE_IMAGE", "/opt/eurovnc/base.img")
OVMF             = os.environ.get("EUROVNC_OVMF", "")
TOKEN_DIR        = os.environ.get("EUROVNC_TOKEN_DIR", "/run/eurovnc/tokens")
SESSION_DIR      = os.environ.get("EUROVNC_SESSION_DIR", "/run/eurovnc/sessions")
SIGNUP_LOG       = os.environ.get("EUROVNC_SIGNUP_LOG", "/var/lib/eurovnc/signups.log")
QEMU             = os.environ.get("EUROVNC_QEMU", "qemu-system-x86_64")
VM_RAM           = os.environ.get("EUROVNC_VM_RAM", "256M")

EMAIL_RE = re.compile(r"^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,63}$")

OVMF_CANDIDATES = (
    "/usr/share/ovmf/OVMF.fd",
    "/usr/share/OVMF/OVMF.fd",
    "/usr/share/edk2-ovmf/x64/OVMF.fd",
)


def find_ovmf():
    if OVMF and os.path.exists(OVMF):
        return OVMF
    for p in OVMF_CANDIDATES:
        if os.path.exists(p):
            return p
    return None


# ----------------------------------------------------------------------------
# Session table
# ----------------------------------------------------------------------------
class Session:
    __slots__ = ("token", "email", "ip", "display", "port", "proc",
                 "overlay", "created", "expires")

    def __init__(self, token, email, ip, display, port, proc, overlay, now):
        self.token = token
        self.email = email
        self.ip = ip
        self.display = display
        self.port = port
        self.proc = proc
        self.overlay = overlay
        self.created = now
        self.expires = now + SESSION_TTL


class Orchestrator:
    def __init__(self):
        self.lock = threading.Lock()
        self.sessions = {}          # token -> Session
        self.last_launch_by_ip = {} # ip -> monotonic ts
        self.ovmf = find_ovmf()
        os.makedirs(TOKEN_DIR, exist_ok=True)
        os.makedirs(SESSION_DIR, exist_ok=True)
        os.makedirs(os.path.dirname(SIGNUP_LOG), exist_ok=True)

    # -- port allocation ---------------------------------------------------
    def _free_display(self):
        used = {s.display for s in self.sessions.values()}
        for d in range(VNC_DISPLAY_MIN, VNC_DISPLAY_MAX + 1):
            if d in used:
                continue
            port = 5900 + d
            # make sure nothing else holds the port
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
                try:
                    s.bind(("127.0.0.1", port))
                except OSError:
                    continue
            return d, port
        return None, None

    # -- launch ------------------------------------------------------------
    def launch(self, email, ip, user_agent):
        email = (email or "").strip()
        if not EMAIL_RE.match(email) or len(email) > 254:
            return 400, {"ok": False, "error": "invalid_email"}
        if not self.ovmf:
            return 500, {"ok": False, "error": "server_no_firmware"}
        if not os.path.exists(BASE_IMAGE):
            return 500, {"ok": False, "error": "server_no_image"}

        now = time.monotonic()
        with self.lock:
            if len(self.sessions) >= MAX_SESSIONS:
                return 503, {"ok": False, "error": "at_capacity",
                             "retry_after": 60}
            # one active session per IP + cooldown
            for s in self.sessions.values():
                if s.ip == ip:
                    return 429, {"ok": False, "error": "one_per_ip",
                                 "url": "/vnc/" + s.token}
            last = self.last_launch_by_ip.get(ip, 0)
            if now - last < IP_COOLDOWN:
                return 429, {"ok": False, "error": "cooldown",
                             "retry_after": int(IP_COOLDOWN - (now - last))}

            display, port = self._free_display()
            if display is None:
                return 503, {"ok": False, "error": "at_capacity",
                             "retry_after": 60}

            token = secrets.token_urlsafe(18)
            overlay = os.path.join(SESSION_DIR, token + ".qcow2")
            try:
                self._make_overlay(overlay)
                proc = self._spawn_qemu(overlay, display)
            except Exception as exc:  # noqa: BLE001 - surface as 500
                self._safe_unlink(overlay)
                return 500, {"ok": False, "error": "launch_failed",
                             "detail": str(exc)[:200]}

            self._write_token_file(token, port)
            sess = Session(token, email, ip, display, port, proc, overlay, now)
            self.sessions[token] = sess
            self.last_launch_by_ip[ip] = now

        self._log_signup(email, ip, token, user_agent)
        return 200, {"ok": True, "token": token, "url": "/vnc/" + token,
                     "expires_in": SESSION_TTL}

    def _make_overlay(self, overlay):
        # copy-on-write overlay on the read-only base image; guest writes are
        # discarded when the session ends.
        subprocess.run(
            ["qemu-img", "create", "-q", "-f", "qcow2",
             "-b", BASE_IMAGE, "-F", "raw", overlay],
            check=True, timeout=30,
        )

    def _spawn_qemu(self, overlay, display):
        args = [
            QEMU, "-machine", "q35", "-m", VM_RAM,
            "-cpu", "qemu64,+smep,+smap",
            "-bios", self.ovmf,
            "-drive", "format=qcow2,file=%s" % overlay,
            "-vnc", "127.0.0.1:%d" % display,
            "-display", "none",
            "-nodefaults", "-vga", "std",
            # USB input so the VNC session has a keyboard and an ABSOLUTE pointer
            # (usb-tablet) — the cursor tracks the browser pointer exactly instead
            # of drifting like a relative PS/2 mouse.
            "-device", "qemu-xhci,id=xhci",
            "-device", "usb-kbd,bus=xhci.0",
            "-device", "usb-tablet,bus=xhci.0",
            "-no-reboot",
            # no -netdev at all: the guest has NO network path out.
            "-sandbox", "on,obsolete=deny,elevateprivileges=deny,"
                        "spawn=deny,resourcecontrol=deny",
        ]
        return subprocess.Popen(
            args, stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            start_new_session=True,
        )

    def _write_token_file(self, token, port):
        # websockify TokenFile format:  <token>: host:port
        path = os.path.join(TOKEN_DIR, token)
        tmp = path + ".tmp"
        with open(tmp, "w") as fh:
            fh.write("%s: 127.0.0.1:%d\n" % (token, port))
        os.replace(tmp, path)

    def _log_signup(self, email, ip, token, user_agent):
        line = "\t".join([
            time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            ip, email, token, (user_agent or "").replace("\t", " ")[:200],
        ]) + "\n"
        try:
            with open(SIGNUP_LOG, "a") as fh:
                fh.write(line)
        except OSError as exc:
            sys.stderr.write("signup-log write failed: %s\n" % exc)

    # -- teardown ----------------------------------------------------------
    def _safe_unlink(self, path):
        try:
            os.unlink(path)
        except OSError:
            pass

    def _kill(self, sess):
        try:
            os.killpg(os.getpgid(sess.proc.pid), signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            pass
        try:
            sess.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(os.getpgid(sess.proc.pid), signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        self._safe_unlink(os.path.join(TOKEN_DIR, sess.token))
        self._safe_unlink(sess.overlay)

    def reap(self):
        now = time.monotonic()
        drop = []
        with self.lock:
            for tok, sess in list(self.sessions.items()):
                dead = sess.proc.poll() is not None
                if dead or now >= sess.expires:
                    drop.append(sess)
                    del self.sessions[tok]
        for sess in drop:
            self._kill(sess)

    def shutdown(self):
        with self.lock:
            sessions = list(self.sessions.values())
            self.sessions.clear()
        for sess in sessions:
            self._kill(sess)

    def status(self):
        with self.lock:
            now = time.monotonic()
            return {
                "active": len(self.sessions),
                "max": MAX_SESSIONS,
                "sessions": [
                    {"token": s.token[:6] + "…", "email": s.email,
                     "remaining": max(0, int(s.expires - now))}
                    for s in self.sessions.values()
                ],
            }


ORCH = None  # set in main()


def reaper_loop():
    while True:
        time.sleep(15)
        try:
            ORCH.reap()
        except Exception as exc:  # noqa: BLE001
            sys.stderr.write("reaper error: %s\n" % exc)


# ----------------------------------------------------------------------------
# HTTP handler
# ----------------------------------------------------------------------------
class Handler(BaseHTTPRequestHandler):
    server_version = "eurovnc/1.0"
    protocol_version = "HTTP/1.1"

    def _client_ip(self):
        # nginx sets X-Real-IP / X-Forwarded-For; fall back to socket peer.
        xff = self.headers.get("X-Real-IP") or self.headers.get(
            "X-Forwarded-For", "")
        if xff:
            return xff.split(",")[0].strip()
        return self.client_address[0]

    def _send_json(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/api/status":
            self._send_json(200, ORCH.status())
        elif self.path == "/api/health":
            self._send_json(200, {"ok": True})
        else:
            self._send_json(404, {"ok": False, "error": "not_found"})

    def do_POST(self):
        if self.path != "/api/launch":
            self._send_json(404, {"ok": False, "error": "not_found"})
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            length = 0
        if length <= 0 or length > 4096:
            self._send_json(400, {"ok": False, "error": "bad_request"})
            return
        raw = self.rfile.read(length)
        ctype = self.headers.get("Content-Type", "")
        email = ""
        try:
            if "application/json" in ctype:
                email = (json.loads(raw or b"{}") or {}).get("email", "")
            else:  # form-encoded
                from urllib.parse import parse_qs
                email = parse_qs(raw.decode("utf-8", "replace")).get(
                    "email", [""])[0]
        except (ValueError, UnicodeError):
            self._send_json(400, {"ok": False, "error": "bad_request"})
            return
        code, obj = ORCH.launch(email, self._client_ip(),
                                self.headers.get("User-Agent", ""))
        self._send_json(code, obj)

    def log_message(self, fmt, *args):  # quieter logs
        sys.stderr.write("%s - %s\n" % (self._client_ip(), fmt % args))


def main():
    global ORCH
    ORCH = Orchestrator()
    if not ORCH.ovmf:
        sys.stderr.write("WARNING: no OVMF firmware found; launches will fail\n")

    threading.Thread(target=reaper_loop, daemon=True).start()

    httpd = ThreadingHTTPServer((LISTEN_HOST, LISTEN_PORT), Handler)

    def _sigterm(_signo, _frame):
        sys.stderr.write("shutting down, killing %d session(s)\n"
                         % len(ORCH.sessions))
        ORCH.shutdown()
        httpd.shutdown()

    signal.signal(signal.SIGTERM, _sigterm)
    signal.signal(signal.SIGINT, _sigterm)
    sys.stderr.write("eurovnc orchestrator on %s:%d  (max=%d ttl=%ds)\n"
                     % (LISTEN_HOST, LISTEN_PORT, MAX_SESSIONS, SESSION_TTL))
    try:
        httpd.serve_forever()
    finally:
        ORCH.shutdown()


if __name__ == "__main__":
    main()
