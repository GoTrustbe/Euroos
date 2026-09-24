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

### W1b. done: the mouse press fires the button

The delivered bytes showed `state=0` on both press and release. X defines
`state` as the mask BEFORE the event, so a release must carry Button1Mask;
chrome derives a release's flags from it and a Views button fires only when
the release carries the left-button flag. Fixed in send_input (33d9a1c). The
screendump at 200 s of run 13, before any Enter, shows the dialog gone:
`docs/proof/2026-09-24-desktop-dialog-gone-after-click-alone.png`.

### W1c. done: the tab navigates again after the dialog

The startup navigation is lost while the dialog interrupts startup (target
url "" and no title). The input-only bridge re-issues Page.navigate once at
its fourth heartbeat; Page.loadEventFired follows, title "Chromium on EuroOS".

### EXIT CRITERION 1 MET (run 13, 33d9a1c)

`docs/proof/2026-09-24-desktop-chromium-renders-euro-html.png`: typed
`chrome`, dialog dismissed by the scripted click, euro.html painted in full in
the desktop window at 660 s, no one at the keyboard, on the NUC.

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

### W3. done for four children; a fifth still fails

Measured cause and effect (run 12b): the fourth fork fails at "pool has 127
MiB" and eleven lines later chrome reports "Target crashed" for every pending
command; after the dialog, chrome navigates in a NEW renderer, and without a
fourth arena the tab is dead. With the guest at 4608M (runbook default now)
the 1152 MiB candidate is taken and the fourth child forks (a second
renderer). A fifth fork still fails late in the run. Arenas ARE recycled on
child exit and on kill(); the superseded first renderer never exits, so its
arena is never returned. Next: find out what the fifth child is, and whether
the superseded renderer should be reaped (chrome sends no kill for it).

### W4. mostly done: the rest of the ENOSYS census

Done (559f064): `link`/`linkat` (a copy in the flat VFS; the caller was
fontconfig's atomic cache publish, not the disk cache), `inotify_init`/`_init1`
as a never-firing eventfd plus add/rm_watch, `getrusage` as zeros. Left, all
benign probes: `recvmmsg` x3, `sigaltstack` x3 (crashpad, disabled),
`name_to_handle_at` x2, `landlock` x1.

### W5. next: the live site. The guest's clock stops after the connect

Peeled in five runs, each layer named by a measurement:
1. `MAX_SOCK = 16` (net.rs): the browser's background traffic used all 16 AF_INET
   slots in minutes, and every later socket() returned -1, read by chrome as
   EPERM: ERR_NAME_NOT_RESOLVED on the resolver's UDP socket, ERR_ACCESS_DENIED
   on a connect. Raised to 96 (fd bands are 100 wide). Two stale pins of the
   old server (151.240.77.50, VFS /etc/hosts and two chrome flags) fixed on the
   way; not the cause.
2. With sockets, TCP to 82.192.72.16:443 establishes and the server FINs within
   50 ms of guest time, no data (run 19).
3. Server-side tcpdump (run 22): the SYN, SYN-ACK and the guest's ACK complete
   in 20 ms; then NOTHING from the guest for 30 s; nginx's stream router
   (ssl_preread, preread_timeout 30 s) sends FIN; 20 ms later the guest's
   ClientHello (1836 B, a correct TLS record) arrives and gets RST.
4. The guest's own clocks agree: between "TCP established" and the FIN the tick
   counter advanced 2 and chrome's wall clock 0.1 s, over 30 real seconds.
   Before the navigate the tick rate was 99.8/s for 300 s. So after the
   connect the guest is halted and only NIC interrupts wake it; the periodic
   LAPIC timer (vector 0x20, class 2) no longer fires while MSI-X 0x4B still
   does, which is the signature of an interrupt of class 2..3 left in service.
5. In the tree (run 23): a probe in the NIC interrupt handler prints the LAPIC
   in-service bits 0x20..0x3f, the timer LVT and its current count whenever
   fewer than 5 ticks passed since the previous NIC interrupt.

### W5b. open: the resolver path itself

Why the system-resolver path returns NAME_NOT_RESOLVED without querying, and
what the DNS config service needs (netlink route socket, or a DnsConfig it
accepts without one). The CreatePlatformSocket EPERM lines (24 in run 16,
all from one thread) belong in this investigation too.

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
