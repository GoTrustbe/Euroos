#!/usr/bin/env python3
"""Boot the EuroKernel image, wait for the desktop, and INJECT mouse movements +
clicks via QMP as RELATIVE events (the kernel has relative pointers only:
PS/2 + USB boot mouse). The cursor starts at the screen center (the kernel sets
mx=width/2, my=height/2); we keep a virtual position and move in small steps.

First makes <prefix>-0.png (desktop at rest), then <prefix>-N.png after each click.

Usage:  WAIT=520 python3 scripts/click-shot.py <img> <prefix> "x,y;x,y;..."
"""
import json, os, select, socket, subprocess, sys, time

IMG = sys.argv[1] if len(sys.argv) > 1 else "eurokernel.img"
PREFIX = sys.argv[2] if len(sys.argv) > 2 else "/tmp/click"
CLICKS = sys.argv[3] if len(sys.argv) > 3 else ""
WAIT = int(os.environ.get("WAIT", "520"))
OVMF = "/usr/share/ovmf/OVMF.fd"
QMP = "/tmp/ek-qmp.sock"
SCREEN_W, SCREEN_H = 1920, 1080

try:
    os.unlink(QMP)
except FileNotFoundError:
    pass

qemu = subprocess.Popen([
    "qemu-system-x86_64", "-machine", "q35",
    "-cpu", "qemu64,+smep,+smap", "-smp", "1", "-m", "256M",
    "-bios", OVMF, "-drive", f"format=raw,file={IMG}",
    "-display", "none", "-serial", "file:serial.log",
    "-qmp", f"unix:{QMP},server,nowait",
    "-netdev", "user,id=n0,ipv4=on,ipv6=on",
    "-device", "virtio-net-pci,netdev=n0,disable-modern=on",
    # USB mouse: the kernel supports a USB boot mouse via xHCI (apply_usb), which
    # the PS/2 route in this headless QMP setup did not do reliably. Relative
    # input-send-event events are routed here.
    "-device", "qemu-xhci,id=xhci",
    "-device", "usb-kbd,bus=xhci.0",  # USB keyboard: kernel reads via xHCI HID (run-e2e method)
    "-no-reboot",
], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

for _ in range(50):
    if os.path.exists(QMP):
        break
    time.sleep(0.2)

s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(QMP)
f = s.makefile("rwb", buffering=0)


def cmd(obj):
    f.write((json.dumps(obj) + "\n").encode())
    # Skip asynchronous events (RTC_CHANGE, …); wait for the actual command reply.
    while True:
        line = json.loads(f.readline().decode())
        if "event" in line:
            continue
        return line


f.readline()  # QMP greeting
cmd({"execute": "qmp_capabilities"})
def drain_qmp():
    # CRITICAL: drain the QMP socket of asynchronous events while we wait.
    # If we don't, QEMU's QMP buffer fills up and FREEZES the whole VM
    # (the guest otherwise hung at ~2s guest time → black screendump).
    while select.select([s], [], [], 0)[0]:
        if not f.readline():
            break


# Wait DELIBERATELY: poll serial.log until the desktop is actually rendered (+ keep QMP
# running) instead of blindly sleeping WAIT seconds. Saves a lot of wait time under TCG.
print(f"[click-shot] booting, poll serial until desktop (max {WAIT}s)...", flush=True)
MARKER = "interactieve loop gestart"
start = time.time()
deadline = start + WAIT
rendered = False
while time.time() < deadline:
    drain_qmp()
    try:
        with open("serial.log", "rb") as sf:
            if MARKER.encode() in sf.read():
                rendered = True
                break
    except FileNotFoundError:
        pass
    time.sleep(1)
# Give the first full render a few more seconds to draw (+ keep draining).
for _ in range(8 if rendered else 0):
    drain_qmp()
    time.sleep(1)
print(f"[click-shot] desktop rendered={rendered} after ~{int(time.time()-start)}s", flush=True)

# Virtual cursor position = screen center (as the kernel initializes it).
vx, vy = SCREEN_W // 2, SCREEN_H // 2


def rel(dx, dy):
    cmd({"execute": "input-send-event", "arguments": {"events": [
        {"type": "rel", "data": {"axis": "x", "value": dx}},
        {"type": "rel", "data": {"axis": "y", "value": dy}},
    ]}})


def move_to(tx, ty):
    global vx, vy
    # Move in steps of max 60px so the PS/2 emulation turns each delta neatly
    # into packets and the kernel keeps up.
    while vx != tx or vy != ty:
        dx = max(-60, min(60, tx - vx))
        dy = max(-60, min(60, ty - vy))
        rel(dx, dy)
        vx += dx
        vy += dy
        time.sleep(0.03)


def click_at(tx, ty):
    move_to(tx, ty)
    time.sleep(0.2)
    cmd({"execute": "input-send-event", "arguments": {"events": [
        {"type": "btn", "data": {"button": "left", "down": True}}]}})
    time.sleep(0.2)
    cmd({"execute": "input-send-event", "arguments": {"events": [
        {"type": "btn", "data": {"button": "left", "down": False}}]}})
    time.sleep(1.6)


def shot(path):
    r = cmd({"execute": "screendump", "arguments": {"filename": os.path.abspath(path), "format": "png"}})
    print(f"[click-shot] {path}: {r}", flush=True)


# Keyboard injection via QMP send-key (PS/2; reliable — the shell is interactive).
# A token is a qcode or "shift+<qcode>" for symbols that require shift.
QK = {
    "1": "1", "2": "2", "3": "3", "4": "4", "5": "5",
    "6": "6", "7": "7", "8": "8", "9": "9", "0": "0",
    "+": "shift+equal", "-": "minus", "*": "shift+8", "/": "slash",
    "(": "shift+9", ")": "shift+0", ".": "dot", "=": "equal",
    ":": "shift+semicolon", "\n": "ret",
}
# Letters a-z (qcode = the letter itself).
for _c in "abcdefghijklmnopqrstuvwxyz":
    QK[_c] = _c


def send_key(token):
    # Hold each key down ~0.4s so the slow USB-HID poll (xHCI) is sure to
    # see it — otherwise keystrokes get dropped under TCG. Modifiers down first, up last.
    parts = token.split("+")  # e.g. ["shift","8"]
    def k(q, down):
        return {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": q}}}
    cmd({"execute": "input-send-event", "arguments": {"events": [k(q, True) for q in parts]}})
    time.sleep(0.4)
    cmd({"execute": "input-send-event", "arguments": {"events": [k(q, False) for q in reversed(parts)]}})
    time.sleep(0.5)


def type_expr(expr):
    for ch in expr:
        if ch in QK:
            print(f"[click-shot] key '{ch}'", flush=True)
            send_key(QK[ch])
    # Wait after Enter so a (blocking) real fetch+render can complete.
    after = int(os.environ.get("EK_WAIT_AFTER", "3"))
    print(f"[click-shot] waiting {after}s for fetch/render...", flush=True)
    time.sleep(after)


shot(f"{PREFIX}-0.png")
n = 1
for chunk in CLICKS.split(";"):
    chunk = chunk.strip()
    if not chunk:
        continue
    xs, ys = chunk.split(",")
    print(f"[click-shot] click {n} -> ({xs},{ys})", flush=True)
    click_at(int(xs), int(ys))
    shot(f"{PREFIX}-{n}.png")
    n += 1

# Optional: type an expression via the keyboard, then a final shot.
typ = os.environ.get("EK_TYPE", "")
if typ:
    print(f"[click-shot] typing: {typ}", flush=True)
    type_expr(typ)
    shot(f"{PREFIX}-typed.png")

cmd({"execute": "quit"})
time.sleep(1)
qemu.terminate()
try:
    qemu.wait(timeout=5)
except Exception:
    qemu.kill()
print("[click-shot] done.", flush=True)
