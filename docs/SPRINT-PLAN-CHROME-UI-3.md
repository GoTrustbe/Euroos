# Sprint 3: input, and a browser you open

Sprint 2 ended with the complete Chromium UI painted on the EuroOS framebuffer by
chrome itself. Two things were missing before it could be called a browser: it did not
react to a keyboard or a mouse, and it lived in the boot phase instead of on the
desktop.

## What was actually wrong (measured, not guessed)

**1. No input reached the guest at all.** Every click and key injected through the
qemu monitor produced nothing. Not a missed hit, not the wrong window: no event. The
kernel's own counters, added to the launcher heartbeat, answered it in one boot:
`kbd-irq=1 mouse-irq=1` for an entire run, with the pointer parked where it booted.
This guest gets no PS/2 interrupts under `-display none`, and the harness gave it no
USB controller either (`[xhci] no xHCI controller found`), so the HID harvest that
runs off the timer tick had nothing to harvest.

Fix: the harness attaches `qemu-xhci` + `usb-kbd` + `usb-tablet`, and
`scripts/qmp-input.py` drives input over QMP. The tablet is ABSOLUTE, so a click lands
where the screenshot says it should instead of wherever a relative mouse has drifted.
Both enumerate: *slot 1: HID keyboard configured → live*, *slot 2: HID tablet
(absolute) configured → live*.

**2. The coordinates described a screen nobody was looking at.** The X server blits a
window centred and integer-scaled: chrome's 800x600 browser window lands at (560,240)
at scale 1, and its 400x122 profile dialog is magnified 4x to 1600x488 at (160,296).
The pump translated the pointer with the window's own X coordinate (40,40), so every
event missed by the difference — and no screenshot could show why.

Fix: one `screen_place()` decides where an image lands, and both the blit and the
input routing read it. The server keeps a presentation table (which window's pixels
are where, and in what order they were drawn); a pointer event goes to the newest
window containing the pointer, at the coordinate that window sees.

**3. Events that were not events.** The button arrived as a one-shot press latch with
no ButtonRelease at all, so a toolkit believed the button was still held and no click
ever completed. The `state` field was always 0 (no shift/ctrl/alt, no held button),
root and event coordinates were the same number written twice, and a keystroke went to
every window at once.

**4. A click sampled instead of queued.** Reading the button LEVEL from the pump loses
an entire click whenever that loop does not happen to run during the ~100 ms the
button is down — and while chrome has the CPU, that is most clicks. Button edges are
now queued in the driver, like scancodes: a click can be late, never lost.

**5. Requests the server answered with silence.** `GrabPointer`/`GrabKeyboard` expect
a reply; chrome makes both (menus, drags, its modal dialogs) and got none, which parks
the client on a reply that never comes. `SetInputFocus` was ignored, so nothing ever
had focus. `WarpPointer` did not move the pointer. `UnmapWindow`/`DestroyWindow` were
not handled at all, so a dismissed dialog stayed a click target — and stayed on
screen — for the rest of the session. Retiring a window now also clears its screen
area, because the fullscreen blit only ever paints.

## The evidence

With the click delivered to the dialog at its own coordinates, chrome's next requests
were the answer:

```
-> input kind=4 detail=1 window=0x400008     (ButtonPress on the dialog)
-> input kind=5 detail=1 window=0x400008     (ButtonRelease)
UnmapWindow id=0x400008
DestroyWindow id=0x400008                    (chrome dismissed its own dialog)
```

## A browser you open

`spawn_glibc_disk_persistent` runs the 485 MB binary, demand-paged from the EuroPack
disk, ALONGSIDE the desktop instead of being waited on. It shares one launch path with
`run_glibc_disk` (`glibc_disk_launch`), so the two lifecycles cannot drift apart, and
the kill path returns the demand pool as well as the arena — without that the window
closes but the memory does not come back. `chrome` in the terminal starts it, `chrome
stop` or the window's close button ends it, and the app launcher lists "Chromium
browser". The boot-phase run is the iteration harness only now, behind the
`chrome-boot` feature (`scripts/build.sh chrome`).

## Harness
- `scripts/chrome-ui-input.sh` — boot, screendump the painted UI, inject a real input
  script, screendump again. "Chrome reacts" is a diff between two images.
- `scripts/qmp-input.py` — `move X Y` / `click` / `key NAME` / `wait S` / `shot PATH`.
- `scripts/chrome-desktop.sh` — boot to the desktop, type `chrome`, sample the screen.

## Open
- In WINDOWED mode the frame shows whichever window presented last, so a dialog and
  the browser take turns owning it.
- The keyboard reaches the focused window; chrome's own keymap path (xkb) is not
  exercised yet, so typing into the address bar is unproven.
- Chrome busy-loops on `recvmsg` after the dialog closes, which starves the launcher
  loop; the click queue makes that survivable rather than solved.
