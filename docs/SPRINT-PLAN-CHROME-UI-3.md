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

## The desktop milestone

`chrome` typed into the Terminal on the live desktop:

```
euroos:/ $ chrome
chrome: launched (task 7) — the window paints as it starts up
```

and the screendump three minutes later shows a first-class EuroOS window titled
"Chromium  -  chrome" — traffic lights, the Protected badge, the browser painting
inside it — next to the Terminal that started it. Chromium is an application on this
desktop, not a boot phase.

One thing had to die for that: the ggtk demo's self-test closes the hosted X window
(and kills the persistent process) 90 desktop ticks after it appears, to prove the
teardown path. It shot down a live browser. It runs with its demo now, or not at all.

## Harness
- `scripts/chrome-ui-input.sh` — boot, screendump the painted UI, inject a real input
  script, screendump again. "Chrome reacts" is a diff between two images.
- `scripts/qmp-input.py` — `move X Y` / `click` / `key NAME` / `wait S` / `shot PATH`.
- `scripts/chrome-desktop.sh` — boot to the desktop, type `chrome`, sample the screen.

## What the clicks finally revealed

Clicks aimed at the tab strip changed nothing, and four runs of asking the right
question turned that into a chain of separate facts:

1. The X server now reports the queue an event lands in. It grew — 32, 64, 96, 128,
   160 bytes — with no client read. The events were not being collected.
2. So the server dumps every thread the moment that happens. The dump named the cause:
   `ring-3 page fault addr=0x7 -> process pid 0 (task 7) TERMINATED`. Chrome's MAIN
   THREAD had died BEFORE the click was ever delivered; the UI on screen was the last
   frame it left behind. Every "the click does nothing" reading was really "there is
   nobody left to click on".
3. What the crash follows: hundreds of `ENOSYS Linux syscall 27` per second. That is
   mincore, which chrome's memory-infra dump calls for every dump. Implemented (our
   pages are mapped or faulted in on touch and nothing swaps, so a valid range is
   resident) — and the main-thread crash is gone.
4. With the main thread alive, the events STILL sit unread. The wait diagnostic says
   why nobody collects them: the only thread polling an X connection is
   `VizCompositorThread` on its own (empty) connection, while the browser's connection
   is fd603 — and the main thread's last syscall is a `writev` to exactly that fd,
   after which it makes no syscalls at all while staying Ready. It is spinning in user
   space, not waiting.

That spin is the next wall, and it is a different problem from input routing: the
routing is now proven correct end to end, down to the coordinate and the queue.

## A second accidental discovery

Removing 55 000 lines of `[scm] recvmsg` log noise brought back a wall the previous
sprint had already knocked down: "Terminating current process after 15 seconds with no
connection". The print had been throttling chrome's empty-recvmsg poll loop, and
without it the poller starved the thread that establishes the renderer channel. The
loop now yields on purpose. The effect is not subtle: the browser paints its full UI
21 presents into the run, 11 seconds after the window maps, where before it needed
four minutes.

## Open
- In WINDOWED mode the frame shows whichever window presented last, so a dialog and
  the browser take turns owning it.
- The keyboard reaches the focused window; chrome's own keymap path (xkb) is not
  exercised yet, so typing into the address bar is unproven.
- The browser's main thread spins in user space after its last X request instead of
  returning to its event loop. Nothing it does reaches a syscall, so the next step is
  sampling its RIP while it spins and mapping that to a symbol.
