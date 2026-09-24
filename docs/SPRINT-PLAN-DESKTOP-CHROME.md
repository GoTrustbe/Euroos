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

### W1. next: X input is queued for chrome and never read

Evidence (run 8): after the fixed click,
`[xserver] 128 B of input queued unread on connection 0`, and chrome's main
thread `t9` sits at syscall 47 (recvmsg) returning -11 EAGAIN. The X server has
the ButtonPress/Release; chrome polls; nothing marks that AF_UNIX connection
readable. Motion reaches chrome (the OK button paints its hover state), so this
is about readiness on the X connection, not about input.

Plan: find where the X server appends to a connection's outbound queue and
where poll/epoll compute readiness for AF_UNIX fds; the append must raise the
readable state and wake a waiter. Same family as the earlier "socket readiness
in poll/epoll" and "EAGAIN-vs-EOF" fixes. Verify: the dialog closes on the
scripted click (`CLICK_AT="697 456"`), then the page paints.

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

### W4. open: the rest of the ENOSYS census

Cheap ones first, each verified by the census shrinking: `getrusage` (zeros are
honest for what chrome does with it), `inotify_init` (a valid fd that never
fires is truthful: nothing on this VFS changes behind chrome's back),
`name_to_handle_at` and `landlock` (EOPNOTSUPP is the honest answer, chrome
handles it). `sigaltstack` stays ENOSYS until signals on alternate stacks are
real; crashpad is disabled anyway.

### W5. open: TLS handshakes fail on the desktop path

`ssl_client_socket_impl.cc:956 handshake failed ... net_error -100` x10 per
run. The September multi-process runs rendered the live site over https, so
this is new to the desktop path or to this build. Not in scope until a page
renders at all; noted so it is not rediscovered.

### W6. instrument: the flaky wedge

One run in eight went silent after the keystrokes with IRQs still firing.
`/root/wedge-probe.sh` starts a run and injects an NMI if the guest goes quiet.
Keep it in the loop; when it catches one, the probe names the RIP.

## Done this sprint (all on `feature/app-control`, not pushed)

- b875f70 mremap + msync. Shared windows re-aliased, not copied.
- 6f8568c HID report log counted per kind; CLICK_AT in the runbook; narrow vmodule.
- 850546c one shared buffer for four in-flight HID TRBs: clicks now carry the
  button, cursor lands on the dialog's OK, hover state paints.
- scripts/nuc-desktop-run.sh: the KVM runbook, in the repo this time.

## Exit criteria

1. Scripted run: `chrome` typed, dialog dismissed by a scripted click or absent,
   `file:///tmp/euro.html` painted in the window (screendump shows the page).
2. Same run against `https://euro-os.eu/` renders the site.
3. Three consecutive runs pass (the repeatability bar used for multi-process).
