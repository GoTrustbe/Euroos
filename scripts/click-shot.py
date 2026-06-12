#!/usr/bin/env python3
"""Boot de EuroKernel-image, wacht op de desktop, en INJECTEER muisbewegingen +
klikken via QMP als RELATIEVE events (de kernel heeft enkel relatieve pointers:
PS/2 + USB-boot-muis). De cursor start op het scherm-midden (de kernel zet
mx=width/2, my=height/2); we houden een virtuele positie bij en bewegen in stapjes.

Maakt eerst <prefix>-0.png (desktop in rust), dan na elke klik <prefix>-N.png.

Gebruik:  WAIT=520 python3 scripts/click-shot.py <img> <prefix> "x,y;x,y;..."
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
    # USB-muis: de kernel ondersteunt USB-boot-muis via xHCI (apply_usb), wat de
    # PS/2-route in deze headless QMP-opstelling niet betrouwbaar deed. Relatieve
    # input-send-event-events routeren hiernaartoe.
    "-device", "qemu-xhci,id=xhci",
    "-device", "usb-kbd,bus=xhci.0",  # USB-toetsenbord: kernel leest via xHCI-HID (run-e2e-methode)
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
    # Sla asynchrone events (RTC_CHANGE, …) over; wacht op de echte command-reply.
    while True:
        line = json.loads(f.readline().decode())
        if "event" in line:
            continue
        return line


f.readline()  # QMP-greeting
cmd({"execute": "qmp_capabilities"})
def drain_qmp():
    # KRITIEK: leeg de QMP-socket van asynchrone events terwijl we wachten.
    # Doen we dat niet, dan loopt QEMU's QMP-buffer vol en BEVRIEST de hele VM
    # (de gast bleef anders op gasttijd ~2s hangen → zwarte schermdump).
    while select.select([s], [], [], 0)[0]:
        if not f.readline():
            break


# Wacht GERICHT: poll serial.log tot de desktop écht gerenderd is (+ QMP draaiende
# houden) i.p.v. blind WAIT seconden te slapen. Scheelt onder TCG veel wachttijd.
print(f"[click-shot] booting, poll serial tot desktop (max {WAIT}s)...", flush=True)
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
# Geef de eerste volledige render nog een paar seconden om af te tekenen (+ blijf draineren).
for _ in range(8 if rendered else 0):
    drain_qmp()
    time.sleep(1)
print(f"[click-shot] desktop gerenderd={rendered} na ~{int(time.time()-start)}s", flush=True)

# Virtuele cursorpositie = scherm-midden (zoals de kernel initialiseert).
vx, vy = SCREEN_W // 2, SCREEN_H // 2


def rel(dx, dy):
    cmd({"execute": "input-send-event", "arguments": {"events": [
        {"type": "rel", "data": {"axis": "x", "value": dx}},
        {"type": "rel", "data": {"axis": "y", "value": dy}},
    ]}})


def move_to(tx, ty):
    global vx, vy
    # Beweeg in stapjes van max 60px zodat de PS/2-emulatie elke delta netjes
    # in pakketten omzet en de kernel meekomt.
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


# Toetsenbord-injectie via QMP send-key (PS/2; betrouwbaar — de shell is interactief).
# Een token is een qcode of "shift+<qcode>" voor symbolen die shift vereisen.
QK = {
    "1": "1", "2": "2", "3": "3", "4": "4", "5": "5",
    "6": "6", "7": "7", "8": "8", "9": "9", "0": "0",
    "+": "shift+equal", "-": "minus", "*": "shift+8", "/": "slash",
    "(": "shift+9", ")": "shift+0", ".": "dot", "=": "equal",
    ":": "shift+semicolon", "\n": "ret",
}
# Letters a-z (qcode = de letter zelf).
for _c in "abcdefghijklmnopqrstuvwxyz":
    QK[_c] = _c


def send_key(token):
    # Houd elke toets ~0.4s INGEDRUKT zodat de trage USB-HID-poll (xHCI) hem zeker
    # ziet — anders vallen aanslagen weg onder TCG. Modifiers eerst in/laatst uit.
    parts = token.split("+")  # bv. ["shift","8"]
    def k(q, down):
        return {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": q}}}
    cmd({"execute": "input-send-event", "arguments": {"events": [k(q, True) for q in parts]}})
    time.sleep(0.4)
    cmd({"execute": "input-send-event", "arguments": {"events": [k(q, False) for q in reversed(parts)]}})
    time.sleep(0.5)


def type_expr(expr):
    for ch in expr:
        if ch in QK:
            print(f"[click-shot] toets '{ch}'", flush=True)
            send_key(QK[ch])
    # Wacht ná Enter zodat een (blokkerende) echte fetch+render kan voltooien.
    after = int(os.environ.get("EK_WAIT_AFTER", "3"))
    print(f"[click-shot] wacht {after}s op fetch/render...", flush=True)
    time.sleep(after)


shot(f"{PREFIX}-0.png")
n = 1
for chunk in CLICKS.split(";"):
    chunk = chunk.strip()
    if not chunk:
        continue
    xs, ys = chunk.split(",")
    print(f"[click-shot] klik {n} -> ({xs},{ys})", flush=True)
    click_at(int(xs), int(ys))
    shot(f"{PREFIX}-{n}.png")
    n += 1

# Optioneel: typ een expressie via het toetsenbord, dan een eindshot.
typ = os.environ.get("EK_TYPE", "")
if typ:
    print(f"[click-shot] typen: {typ}", flush=True)
    type_expr(typ)
    shot(f"{PREFIX}-typed.png")

cmd({"execute": "quit"})
time.sleep(1)
qemu.terminate()
try:
    qemu.wait(timeout=5)
except Exception:
    qemu.kill()
print("[click-shot] klaar.", flush=True)
