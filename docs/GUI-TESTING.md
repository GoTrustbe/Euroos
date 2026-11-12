# Testing the desktop: `scripts/gui-smoke.py`

The load tests (`scripts/loadtest.py`, `LOADTEST-TRACKER.md`) drive the shell over
the serial console. Until 2026-11 nothing drove the desktop itself: windows, the
dock, the launcher, the in-kernel apps. `scripts/gui-smoke.py` does, with a real
pointer and keyboard.

```bash
# the public image (boots straight to the desktop), TCG: about 8 minutes (3 under KVM)
./scripts/build.sh image
python3 scripts/gui-smoke.py --image eurokernel.img --workdir /tmp/gui-smoke-1

# KVM, then 30 minutes of random input with a fixed seed
python3 scripts/gui-smoke.py --image eurokernel.img --kvm --smp 4 --mem 4G \
    --soak 30 --seed 20261110 --workdir /srv/tmp/gui-soak-1
```

Exit status 0 = every step passed, 1 = a step failed, 2 = the guest crashed or
hung. The work directory holds `report.txt` / `report.json`, `serial.log`,
`actions.log` (every injected event, in order), `qemu.cmdline`, and the
screenshots (`app-NN-<name>.png` per dock app, `feature-*.png`, `monkey-NN.png`).

## How it drives the guest

- **Input** goes through QMP `input-send-event` to a USB tablet and USB keyboard
  on xHCI (`--input virtio`: virtio-tablet and virtio-keyboard). The tablet is
  absolute, so a click lands on the pixel the screenshot shows. PS/2 is not used:
  under `-display none` the kernel gets no PS/2 interrupts.
- **The keyboard layout** is US QWERTY on the public image and be-azerty on a
  selftest image (the kernel prints which); the script picks its key table from
  that line. `--layout` overrides.
- **Windows are found on the screenshot**, not by coordinates copied from the
  kernel: a window is where the red, yellow and green title-bar buttons sit
  20 px apart on one row. From that origin the script derives the close button,
  the title bar to drag, and points inside the body. Only the dock geometry
  (`compositor.rs`: tile i at x 45, y 99 + 50 i) and the three button colours are
  constants; if those change in the compositor, change them at the top of the
  script.
- **Everything is private to the run**: the image is copied into the work
  directory, the QMP and serial sockets live there, and the script stops only
  the QEMU process it started (its PID is in `qemu.pid`). The shared
  `/tmp/ek-qmp.sock` of the older scripts is not used. Network: none by default
  (`-nic none`), `--net restrict` (virtio-net on SLIRP with `restrict=on`), or
  `--net e1000e` (an e1000e NIC on SLIRP with `restrict=on`: the NIC the
  userspace driver `e1000d` serves, so that driver's syscalls run beside
  everything the desktop does). More devices go in with
  `--extra "-device intel-hda ..."`.

## What "alive" means

After every step:

1. no crash signature on the serial console (`KERNEL PANIC`, `panicked at`,
   `DOUBLE FAULT`, `GENERAL PROTECTION FAULT`, `[idt] PAGE FAULT`,
   `[idt] INVALID OPCODE`, `memory allocation of N bytes failed`, ...);
2. QEMU still runs;
3. the desktop loop answers: `echo gs-alive-N` sent to the serial shell console
   (`scon`, polled once per loop iteration) comes back as `[scon] gs-alive-N`
   within the timeout (30 s under KVM, 180 s under TCG). The longest round trip
   of the run is in the report; one over 5 s is listed as a stall;
4. where the step must change the screen (a window opens, moves, closes, the
   launcher or a menu appears), two screendumps differ and the window is found
   or gone.

A command typed into the terminal with the injected keyboard must also appear
on the serial console (`[e2e] $ echo gui-smoke-term`): that covers key →
scancode → layout → shell → output, which the serial console alone cannot.

On a crash or hang the script injects an NMI (the kernel prints its NMI probe),
keeps six more seconds of serial output, takes a last screenshot (`fatal.png`)
and stops.

## The smoke pass

1. Boot to `[desktop] interactive loop started` and `[scon-ready]`.
   On the release image the first-run tour is up then (`[tour] first-run tour
   shown`): Esc must close it (`[tour] closed`), which is a step of its own. The
   self-test image and an image with `ui.tour=off` show no tour and have one
   step fewer (55 instead of 56).
2. Close every window the desktop boots with (terminal, install wizard).
3. For each of the eleven dock tiles (files, notes, clock, browser, terminal,
   settings, store, star, text, monitor, log): click the tile, expect a window;
   use the app (clicks in the body, typing, navigation keys; in the terminal a
   command whose echo is checked on serial); screenshot; drag the window by its
   title bar and expect it at the new place; click its close button and expect
   it gone.
4. Three apps open together; Alt-Tab (the "Switch window" panel appears, goes
   away when Alt is released, a window is raised); the same with EuroText in
   front; Alt+2 (empty workspace) and Alt+1 (windows back); the launcher
   opened with the EU mark, `term` typed, Enter, and a command run in the
   terminal that opens; a right click on the bare desktop (context menu).

5. Since 2026-11-12 (ten more steps, 66 on the release image): Esc closes the
   context menu; the notification list opens on a click on the status panel,
   closes with Esc and with a click beside it, and while it is open the desktop
   repaints on change (the `repaints` shell command is read twice, 3 s apart:
   at most one full repaint per ten loop iterations; BUG-057); AltGr gives
   `@ # { } [ ] ~ € \ |` on be-azerty, fr-azerty and de-qwertz (the layout is
   switched with `keymap` over the serial console, the characters are typed
   into a Terminal command and read back from the `[e2e]` line) and AltGr+Tab
   and AltGr+2 open no switcher and change no workspace, with the Terminal and
   with EuroText in front (BUG-052); the mouse wheel scrolls EuroBeheer down and
   back up, and the Terminal back, where a key returns to the prompt. The same
   steps on the image without the fixes: 59/66
   (`proof/uidebug-2026-11/gui-smoke-kvm-baseline-image.txt`).

6. Since the integration of 2026-11-12 (one more step, 67 on the release
   image): BUG-063 from the desktop. `/bin/hello` is typed in the Terminal: a
   foreground program, which leaves through `sys_exit`, after which the
   Terminal turns interrupts back on. Then `ps`, with `--net e1000e` a
   `ping 10.0.2.2` that has to be answered through `e1000d`, two seconds with
   the desktop idle, `hello` again and an `echo` whose line has to arrive on
   the serial console. Everything the Terminal printed is read from its
   `[e2e]` mirror. Without `--net e1000e` the step runs without the driver
   and says so in its detail.

A step that fails because of a tracked, open bug is listed in `KNOWN_ISSUES`
at the top of the script with its tracker number. It is reported as `known`
and does not fail the run; when the bug is fixed the entry is removed, and the
step counts again. Today the list is empty (BUG-044 was the first entry).

The two Alt-Tab steps take screenshots until the switcher is on screen (up to 4 s,
12 s under TCG): on a busy host one full repaint under TCG can take longer than a
fixed pause.

Under TCG the guest notices a button press some milliseconds late, so a window
that is dragged ends up a few pixels short of the pointer's path. The drag
step therefore accepts a window that followed the pointer for at least half of
the distance; under KVM it lands on the pixel.

The drag waits for the guest twice (`drag_window`). On a busy host under TCG
the guest once latched the press when the pointer was already most of the way,
and the window followed for 37 % of the distance. So after the press the
pointer moves six pixels and stays there until a screenshot shows the window
displaced (up to 6 s, 18 s under TCG): the press is latched. Then the drag is
made, and the button stays down until a screenshot shows the window at least
half of the way (the same wait). What the step demands is unchanged.

## The monkey soak (`--soak MINUTES`)

Random actions from a seeded generator (`--seed`; printed and written to the
report): dock tiles, clicks, right and double clicks anywhere, clicks inside
windows, window drags, drags between random points, title-bar buttons, random
text with and without Enter (now and then a line of a few hundred characters),
navigation and function keys, Alt-Tab, workspace switches, the launcher with a
random query, the status panel, Ctrl+Alt+Esc, and since 2026-11-12 the mouse
wheel at a random place and AltGr chords (a seed therefore gives another
sequence than before that date). About 70 actions a minute under KVM.

- every action: crash scan of the serial console;
- every 10 actions: the liveness check above;
- every 50 actions: the pointer is moved and the picture must follow within
  5 s (15 s under TCG); two checkpoints in a row without that fail the run as a
  frozen screen. (A single look 0.3 s after the move was too strict as long as
  the desktop repainted everything in every iteration with the notification
  list open, 0.3 to 0.4 s per frame under KVM; BUG-057, fixed 2026-11-12.) A
  screenshot is kept;
- every 200 actions: back to workspace 1, the windows that can be found
  closed, the terminal raised through the launcher (`term`, Enter), Enter again
  (to send off whatever the monkey left on the prompt), a command typed and
  checked on serial. Before the click on the EU mark one click goes to the
  strip beside the dock: a context menu or the notification list the monkey
  left open takes the next left click (Esc closes both since BUG-057; the click
  stays for older images). A failed check saves
  `monkey-check-N-failed.png`. A desktop that survives but no longer does work fails this
  step. The launcher is used because it raises and focuses the terminal
  wherever the monkey dragged it: at the bottom or right edge fewer than three
  title-bar buttons are visible and the window cannot be found on a screendump,
  and a dock click on a terminal that is already in front would close it.

The same seed gives the same sequence of actions. Where the sequence looks at
the screen (which window to click in), it depends on what the guest drew, so a
replay follows the original as long as the guest behaves the same; the exact
events that were sent are in `actions.log` either way.

## Looking at the desktop: `probe.py`

`docs/proof/uidebug-2026-11/probe.py` runs a list of input steps with the same
`Guest` class and keeps named screenshots (`dock N`, `wclick DX DY`, `key
alt_r+2`, `wheel 3`, `wshrink`, `scon COMMAND`, `shot NAME`, `expect TEXT`).
`--bootopt display.mode=1280x720` writes the boot option into a private copy
of the image, so the same scenario runs at 1920x1080 and at 1280x720; `--extra`
adds a blank disk for the install wizard. `montage.py` next to it tiles crops
of several screenshots into one picture.

## What it does not cover

- It checks that apps open, take input and do not take the kernel down, and one
  end-to-end result (the terminal). It does not check what each app draws.
- No Chromium pack (that is `scripts/chrome-smoke.py`, which uses this script's `Guest`
  class: `docs/CHROME-ON-EUROOS.md`, "Testen"), no native (ring 3) GUI apps, no network content.
- Window detection needs the three title-bar buttons of a window visible and
  uncovered. A window pushed half off the screen, or with another window over
  its buttons, is not found (the monkey then simply does not pick it).
- The monkey can reach the install wizard and the power controls. It runs on a
  private copy of the image, so nothing outside the work directory is at stake;
  a guest that powers itself off ends the run and is reported as such.
