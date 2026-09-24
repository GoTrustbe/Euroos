# Sprint plan: Chromium as a desktop app, unattended, on real hardware

*Written 2026-09-24 from measurements on the lab NUC (Intel N100, KVM), runbook
`scripts/nuc-desktop-run.sh`. Every item below names the evidence it rests on.
Status labels: done / next / open.*

## Goal

Type `chrome` in the EuroOS Terminal on real hardware and get a rendered page in
the browser window, with no human at the keyboard. Today the window opens, the
EuroGuard badge shows, and the browser stops on a modal it cannot be clicked out
of. The multi-process boot test already renders the live site; this sprint closes
the gap between that and the desktop.

## Method (what earned the results so far, keep doing it)

1. One change per run. Measure before and after, on the same scenario.
2. Let the binary name its blocker. mremap was found because chrome said
   `ENOSYS 25` twelve times, not because anyone guessed.
3. Never read a zero without checking what is allowed to print. Twice now a
   counter measured its own category instead of the system (`REPORTS_LOGGED`,
   and the trace categories before that).
4. A single wedged run proves nothing. Run 3 wedged, the same build ran fine as
   run 4. Use the NMI probe (`/root/wedge-probe.sh` on the NUC, HMP `nmi`) on a
   quiet guest, do not attribute it to the last change.
5. Narrow logging only. `--v=1` was itself the deadlock (stderr convoy); a
   `--vmodule` list of named files is fine.
6. Commit each measured step with what it did NOT fix stated plainly.

## Walls, in the order the evidence points

### W1. done, in two parts: the click reaches chrome; Enter dismisses the dialog

What the instrument in `send_input` settled (run 11, commit c6ed9ff): there is
NO dialog window. The browser connection owns one mapped window, 0x400003,
800x600 at (40,40), all input masks set; the profile dialog is painted inside
it. ButtonPress and ButtonRelease reach it at the right window-local point and
chrome drains them (`[xin] <- conn=0 client DRAINED kind=4/5`). Readiness was
never the problem; the "128 B queued unread" of run 8 was a transient.

**Enter dismisses the dialog** (KEYS_AFTER="ret"), and Chromium's own tab
strip, toolbar and omnibox are then on the EuroOS desktop:
`docs/proof/2026-09-24-desktop-chromium-ui-after-enter.png`.

### W1b. next: the mouse press does not fire the button (hover does)

The delivered bytes show `state=0` on BOTH press and release. X defines
`state` as the button/modifier mask BEFORE the event, so a ButtonRelease must
carry Button1Mask (0x100); chrome derives a release's flags from that word and
a Views button only fires when the release carries the left-button flag. Fix
in the tree (send_input sets the button bit on release, clears it on press);
verify: the scripted click alone dismisses the dialog, no Enter needed.

### W1c. next: the first navigation is dropped; the tab sits at about:blank

After Enter, `Target.getTargets` reports the page target with `url:""` and no
title, and the omnibox shows about:blank, while at startup the same target was
at `file:///tmp/euro.html` (which has a title). The startup navigation was
lost while the dialog interrupted startup. In the tree: the input-only bridge
re-navigates the attached target once, at the fourth heartbeat. Verify: the
page paints in the desktop window (screendump), title "Chromium on EuroOS".

### W2. open: the profile modal itself

"Something went wrong when opening your profile." mremap (b875f70) let SQLite
grow its mmap of `Default/Web Data` (108K -> 148K -> 152K, re-aliased, no copy)
and the modal stayed. `--vmodule` on profile_manager/profile_impl/database/
statement/json_pref_store/profile_error_dialog shows only keep-alive bookkeeping;
the sql layer says nothing at that verbosity. Remaining named gaps in the same
run: `link` (86) x2, `name_to_handle_at` (303) x2, `inotify_init` (253) x2,
`sigaltstack` (131) x3 (crashpad only), `getrusage` (98), `landlock` (444). The
disk cache reports `wrong file structure on disk: 2` and `Unable to create cache`.

Plan, in order: (a) once W1 lands, see whether the dialog is fatal or cosmetic:
if the page renders after OK, W2 drops in priority. (b) `link`: the pref writer
and SimpleCache use hard links and renames; ENOSYS there is a plausible cause
for both the cache structure check and a pref-store failure. Implement, rerun,
read the census. (c) If still present, instrument the VFS side: log every
open/rename/link/mkdir under `/tmp/cr/Default` with its result for one run.

### W3. open: the fork arena pool is exactly full

`[fork] arena alloc FAILED (256 MiB, pool has 127 MiB)` three times per run. The
896 MiB pool holds exactly three 256 MiB arenas; chrome wants a fourth child.
Plan: measure which child is refused (log the argv `--type=` of the failing
fork), then either grow the pool (RAM permits: 7 GiB on the NUC, guest gets
3584M) or give arenas a size class by child type.

### W4. mostly done: the rest of the ENOSYS census

Done (559f064): `link`/`linkat` (a copy in the flat VFS; the caller was
fontconfig's atomic cache publish, not the disk cache), `inotify_init`/`_init1`
as a never-firing eventfd plus add/rm_watch, `getrusage` as zeros. Left, all
benign probes: `recvmmsg` x3, `sigaltstack` x3 (crashpad, disabled),
`name_to_handle_at` x2, `landlock` x1.

### W5. open: TLS handshakes fail on the desktop path

`ssl_client_socket_impl.cc:956 handshake failed ... net_error -100` x10 per
run. The September multi-process runs rendered the live site over https, so
this is new to the desktop path or to this build. Not in scope until a page
renders at all; noted so it is not rediscovered.

### W6. instrument: the flaky wedge, now caught automatically

Two runs in twelve (3 and 12) went silent right after the first keystrokes with
IRQs still firing; the same build ran fine the next time. The runbook now
watches the serial log after typing and, at 60 s of silence, injects an NMI
twice through the monitor and prints the probe (RIP + task census), then keeps
sampling. The next wedge names its own RIP.

## Done this sprint (all on `feature/app-control`, not pushed)

- b875f70 mremap + msync. Shared windows re-aliased, not copied.
- 6f8568c HID report log counted per kind; CLICK_AT in the runbook; narrow vmodule.
- 850546c one shared buffer for four in-flight HID TRBs: clicks carry the button.
- 559f064 link, inotify_init, getrusage.
- c6ed9ff the browser UI on the desktop; Enter dismisses the dialog; the
  send_input census; KEYS_AFTER; 1152 MiB fork-pool candidate (inert at 3584M).
- scripts/nuc-desktop-run.sh: KVM runbook, one-VM guard, NMI wedge watchdog.

## Exit criteria

1. Scripted run: `chrome` typed, dialog dismissed by a scripted click or absent,
   `file:///tmp/euro.html` painted in the window (screendump shows the page).
2. Same run against `https://euro-os.eu/` renders the site.
3. Three consecutive runs pass (the repeatability bar used for multi-process).
