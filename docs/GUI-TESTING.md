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
  (`-nic none`), or `--net restrict` (SLIRP with `restrict=on`). More devices
  go in with `--extra "-device intel-hda ..."`.

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

A step that fails because of a tracked, open bug is listed in `KNOWN_ISSUES`
at the top of the script with its tracker number. It is reported as `known`
and does not fail the run; when the bug is fixed the entry is removed, and the
step counts again. Today: "Alt-Tab with the text editor in front" (BUG-044).

Under TCG the guest notices a button press some milliseconds late, so a window
that is dragged ends up a few pixels short of the pointer's path. The drag
step therefore accepts a window that followed the pointer for at least half of
the distance; under KVM it lands on the pixel.

## The monkey soak (`--soak MINUTES`)

Random actions from a seeded generator (`--seed`; printed and written to the
report): dock tiles, clicks, right and double clicks anywhere, clicks inside
windows, window drags, drags between random points, title-bar buttons, random
text with and without Enter (now and then a line of a few hundred characters),
navigation and function keys, Alt-Tab, workspace switches, the launcher with a
random query, the status panel, Ctrl+Alt+Esc. About 70 actions a minute under
KVM.

- every action: crash scan of the serial console;
- every 10 actions: the liveness check above;
- every 50 actions: the pointer is moved between two screendumps, which must
  differ (a frozen screen fails the run), and a screenshot is kept;
- every 200 actions: back to workspace 1, all windows closed, the terminal
  opened from the dock, Enter (to send off whatever the monkey left on the
  prompt), a command typed and checked on serial. A desktop that survives but
  no longer does work fails this step.

The same seed gives the same sequence of actions. Where the sequence looks at
the screen (which window to click in), it depends on what the guest drew, so a
replay follows the original as long as the guest behaves the same; the exact
events that were sent are in `actions.log` either way.

## What it does not cover

- It checks that apps open, take input and do not take the kernel down, and one
  end-to-end result (the terminal). It does not check what each app draws.
- No Chromium pack, no native (ring 3) GUI apps, no network content.
- The monkey can reach the install wizard and the power controls. It runs on a
  private copy of the image, so nothing outside the work directory is at stake;
  a guest that powers itself off ends the run and is reported as such.
