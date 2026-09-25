#!/usr/bin/env python3
"""Inject REAL input events into a running guest over QMP.

The HMP `sendkey`/`mouse_move` route goes to the PS/2 devices, and this kernel gets
no PS/2 interrupts at all under `-display none` (measured: kbd-irq stays 1 for a whole
boot). USB HID does arrive — the xHCI harvest runs from the timer tick and feeds the
same scancode/mouse paths — and QMP can address a USB tablet with ABSOLUTE
coordinates, so a click lands where the screenshot says it should instead of wherever
a relative mouse happens to have drifted.

Usage: qmp-input.py QMP_SOCKET SCRIPT [screen_w screen_h [HMP_SOCKET]]
Script lines: `move X Y` · `click` · `key NAME` · `wait SECONDS` · `shot PATH`

`key NAME` names a PHYSICAL key (a QEMU qcode), not a letter: what it types depends on
the guest's layout. EuroOS boots the installer default, be-azerty, so `key m` types a
comma and `key semicolon` types the m.
"""
import json, socket, sys, time

sock_path, script_path = sys.argv[1], sys.argv[2]
SW = int(sys.argv[3]) if len(sys.argv) > 3 else 1920
SH = int(sys.argv[4]) if len(sys.argv) > 4 else 1080
HMP = sys.argv[5] if len(sys.argv) > 5 else None

s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sock_path)
f = s.makefile("rw")
f.readline()  # greeting
def cmd(obj):
    f.write(json.dumps(obj) + "\n"); f.flush()
    while True:
        line = f.readline()
        if not line:
            return None
        msg = json.loads(line)
        if "return" in msg or "error" in msg:
            return msg
cmd({"execute": "qmp_capabilities"})

def send(events):
    r = cmd({"execute": "input-send-event", "arguments": {"events": events}})
    if r and "error" in r:
        print("QMP error:", r["error"], flush=True)

def move(x, y):
    # The tablet's logical range is 0..32767 across the screen.
    send([{"type": "abs", "data": {"axis": "x", "value": x * 32767 // max(SW - 1, 1)}},
          {"type": "abs", "data": {"axis": "y", "value": y * 32767 // max(SH - 1, 1)}}])


# Character -> (qcode, shift) on be-azerty. Letters: the physical key that carries
# the letter (a and q, z and w swapped; m sits on the qwerty semicolon key).
# Digits are the shifted top-row keys; ':' is the unshifted qwerty-period key,
# '/' its shift; '.' is shift on the qwerty-comma key; '-' is the qwerty-equal key
# and '_' its shift; '=' the qwerty-slash key; '?' shift on the qwerty-m key.
AZERTY = {}
for c in "bcdefghijklnoprstuvxy":
    AZERTY[c] = (c, False)
    AZERTY[c.upper()] = (c, True)
AZERTY.update({"a": ("q", False), "A": ("q", True), "q": ("a", False), "Q": ("a", True),
               "z": ("w", False), "Z": ("w", True), "w": ("z", False), "W": ("z", True),
               "m": ("semicolon", False), "M": ("semicolon", True)})
for i, c in enumerate("1234567890"):
    AZERTY[c] = (c, True)
AZERTY.update({" ": ("spc", False), ":": ("dot", False), "/": ("dot", True),
               ".": ("comma", True), ";": ("comma", False), ",": ("m", False), "?": ("m", True),
               "-": ("equal", False), "_": ("equal", True), "=": ("slash", False), "+": ("slash", True),
               "&": ("1", False), "!": ("8", False), "(": ("5", False), ")": ("minus", False)})

for raw in open(script_path):
    parts = raw.split()
    if not parts:
        continue
    op = parts[0]
    if op == "move":
        move(int(parts[1]), int(parts[2])); print(f"move {parts[1]},{parts[2]}", flush=True)
    elif op == "click":
        send([{"type": "btn", "data": {"down": True, "button": "left"}}])
        time.sleep(0.15)
        send([{"type": "btn", "data": {"down": False, "button": "left"}}])
        print("click", flush=True)
    elif op == "down":
        send([{"type": "btn", "data": {"down": True, "button": "left"}}])
    elif op == "up":
        send([{"type": "btn", "data": {"down": False, "button": "left"}}])
    elif op == "key":
        send([{"type": "key", "data": {"down": True, "key": {"type": "qcode", "data": parts[1]}}}])
        time.sleep(0.1)
        send([{"type": "key", "data": {"down": False, "key": {"type": "qcode", "data": parts[1]}}}])
        print(f"key {parts[1]}", flush=True)
    elif op == "type":
        # `type TEXT`: the rest of the line as characters, through the be-azerty
        # layout the guest boots with (letters a/q, z/w, m swapped against the
        # qcodes; digits and most URL punctuation live on shifted keys).
        text = raw.split(" ", 1)[1].rstrip("\n") if " " in raw else ""
        for ch in text:
            qc, shift = AZERTY.get(ch, (None, False))
            if qc is None:
                print(f"type: no be-azerty key for {ch!r}, skipped", flush=True)
                continue
            ev = []
            if shift:
                ev.append({"type": "key", "data": {"down": True, "key": {"type": "qcode", "data": "shift"}}})
            ev.append({"type": "key", "data": {"down": True, "key": {"type": "qcode", "data": qc}}})
            send(ev)
            time.sleep(0.06)
            ev = [{"type": "key", "data": {"down": False, "key": {"type": "qcode", "data": qc}}}]
            if shift:
                ev.append({"type": "key", "data": {"down": False, "key": {"type": "qcode", "data": "shift"}}})
            send(ev)
            time.sleep(0.06)
        print(f"type {text!r}", flush=True)
    elif op == "wait":
        time.sleep(float(parts[1]))
    elif op == "shot" and HMP:
        # Screendumps go through the HMP monitor, so one script drives the whole
        # interaction: move, click, look, click again.
        m = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        m.connect(HMP)
        m.sendall(("screendump %s\n" % parts[1]).encode())
        time.sleep(2)
        m.close()
        print(f"SHOT {parts[1]}", flush=True)
    time.sleep(0.4)
print("input script done", flush=True)
