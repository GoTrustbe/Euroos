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

### W5. root cause found: blocking socket paths spin with interrupts off

The freeze is the guest's own doing. Probe v2 in the NIC interrupt handler
(trigger: >1.5 G cycles between NIC interrupts with <20 ticks) caught it twelve
times: a timer interrupt pending in IRR, nothing in service, TPR 0, IF=1, and
the interrupted rip at a syscall's RETURN address in libc. The host-side
sampler (run 25) rules the host out: memory pressure 0.00, four gigabytes
free, the vCPU thread waited 72 ms for a CPU over the whole 843-second run.
So the machine spent those seconds inside a syscall with IF=0, and on sysret
KVM delivered the queued NIC interrupt first and one coalesced timer tick.

The syscalls: `TcpConn::recv` spins up to 80 x pump(8) x poll_seg, and
poll_seg is SPINS*3 = 12 million busy iterations; `UdpSock::recv` (the
resolver) the same 12 million; `TcpConn::send` waits five rounds for an ACK.
Chrome marks every socket O_NONBLOCK and polls them constantly, so every
recv on an idle socket froze the whole guest for seconds: timer, desktop,
the IO thread that wanted to write the ClientHello, everything. The 30-second
gap the server saw before the ClientHello, the "5 ticks" between established
and FIN, the stalls during startup DNS bursts, all the same thing.

Fix (run 26): O_NONBLOCK descriptors take no-wait paths (sock_recv_nowait /
sock_send_nowait: TCP via recv_nowait/send_nowait, UDP a bounded look through
what the NIC already delivered, LocalDns its queue), the desktop loop drives
pump_all so tick() does ACKs and retransmits for the no-wait sends. The
blocking paths stay for blocking descriptors; they still spin and should
yield instead (noted, not needed for chrome).

### W5 progress after the freeze fix (runs 26, 27)

Run 26: zero freezes, ClientHello 33 ms after the SYN, but the navigation
failed with ERR_ACCESS_DENIED again: the 96-slot socket table was full before
the navigate. Cause: a fork child's close() only marks unless the descriptor
is on the child's own list, and socket() never registered its result there;
the network service opens a UDP socket per lookup. Fixed centrally in the
dispatcher (every creating syscall). Run 27: the table never fills, and the
TLS handshake now reaches the server's first flight (4873 B read: ServerHello
and the certificate chain), then nothing for 21 s until the server's FIN and
net::ERR_TIMED_OUT. Chrome sets its epoll interest on the socket to 0 after
the read. Next measurement (run 28): the server-side capture says whether
chrome's second flight ever left the guest.

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

### W5c. runs 28-32: the TLS path is clean; the NSS pack and a self-test were not

Run 28's server capture answered W5's open question: chrome's second flight
never left the guest because a utility process died first with
`libnssutil3.so: version 'NSSUTIL_3.108' not found (required by libsoftokn3.so)`.
Two causes, both outside the kernel's network code: (1) the NSS pack rebuilt on
br-prod took softokn/freebl from the server's NSS 3.120 while chrome-pack2 ships
libnss3 3.98 from noble; `scripts/mk-nss-pack.sh` now builds from the noble
.deb (NSS_DEB=...). (2) The boot self-test "SECOND DISK B3" FORMATTED disk 1
when it found no EuroFS there, and disk 1 on the NUC is the NSS pack: every
first boot with a fresh pack overwrote it past 1 MiB, and the 5 MB noble pack
made the format fail into a kernel panic (run 30). The probe now leaves any
non-blank disk alone and never panics. Runs 31/32: no NSS FATAL, the handshake
completes, and the live navigate at heartbeat 10 gets no answer because the
browser main thread is already dead.

### W7. root cause of the main-thread death: madvise was a no-op

The main thread died at the same instruction in runs 14, 21 and 32
(`addl $1,0x8(%r13)` in `_gtk_css_value_ref`, libgtk-3 + 0x1594ca) with r13 = -1
(page fault at 0x7) or a non-canonical value (#GP), and in run 31 it blocked
forever on a futex; every time at tick ~6779, about 68 s after boot, right after
a burst of mprotect and madvise calls from the same thread in the syscall ring.

That burst is PartitionAlloc's first memory reclaim (a minute after start-up):
it decommits every empty slot span with mprotect + madvise(MADV_DONTNEED). Linux
guarantees such pages read as zeros afterwards, and PartitionAlloc, which is the
malloc of the whole browser process including GTK, relies on it: a zero-fill
allocation (calloc, g_malloc0) served from a recommitted span skips its memset.
The kernel answered madvise with 0 and did nothing, so the recommitted pages
still held the old freelist words. PartitionAlloc encodes freelist pointers as
~ptr, so an encoded NULL is exactly -1 and any other entry is non-canonical:
both shapes of the crash, and the hang is the same garbage in a mutex word.

Fix: madvise(MADV_DONTNEED/MADV_FREE) zeroes every private anonymous demand
page in the range in place (`madvise()` in ring3.rs). Frames stay committed, so
no TLB shootdown is needed; shared frames and file-backed pages keep their
bytes. `[madvise]` lines in the log count the zeroed pages.

Run 33 (commit 233bd9a) verified it: 1984 pages zeroed by tick 4842, not one
fault in 662 s, the main thread answered the live navigate at heartbeat 10, and
the server's access log holds the first fetch of the live site by the desktop
browser on EuroOS:

    euro-os.eu 213.118.185.65 [24/Sep/2026:19:05:04] "GET / HTTP/1.1" 302
    euro-os.eu 213.118.185.65 [24/Sep/2026:19:05:04] "GET /en/ HTTP/1.1" 200 15248

CDP saw responseReceived for the document and then loadingFailed with
net::ERR_ABORTED: the https site needs its own renderer, and the sixth and
seventh fork found "pool has 127 MiB" (W3). The screendump at 480 s still shows
euro.html, with the browser alive.

### W3 continued: seven arenas

Five children hold the 1408 MiB pool's five arenas (utilities 34, 36 and 93,
renderers 49 and 50; none exits, chrome keeps the superseded renderer). Commit
65f3575 puts 1920 and 1664 MiB candidates first and caps the pool at two fifths
of usable RAM: the 4608M guest keeps 1408 MiB and its demand pool, the 5632M
guest (5047 MiB usable) takes 1920 MiB, seven arenas, and still has a larger
demand pool than the 4608M guest had (1115 MiB). The NUC host has 7.6 GB and
the 4608M guest touched 2.8 GB, so the runbook now boots 5632M.

### EXIT CRITERION 2 MET (run 34, 65f3575)

Run 34: the pool is 1920 MiB, the https renderer forks as task 102 right after
the navigate at heartbeat 10, Page.frameNavigated reports https://euro-os.eu/
and the subresources (stylesheet, script, fonts, images, manifest) come in over
three TLS connections (343 KB read by 41 calls at the 480 s mark). The
screendump at 480 s shows the live site in the Chromium window on the EuroOS
desktop: tab title "EuroOS: A sovereign oper...", omnibox euro-os.eu/en/, the
header with GitHub and Download, the hero "An operating system that belongs to
Europe." and the cookie notice. Proof:
`docs/proof/2026-09-24-desktop-chromium-renders-live-site.png` (and
`...-alive-after-reclaim.png`, run 33 at 480 s, the browser alive past the
first reclaim with euro.html still up). The page was still loading at 480 s
(reload button shows the stop cross); loadEventFired is the next thing to read
in the log.

### W5b. root cause: UDP replies were dropped by the wrong socket

Run 34's live page lost one resource, `https://tracera.eu/t.js`, to
ERR_NAME_NOT_RESOLVED, and the log shows why: 31 DNS queries written to
10.0.2.3:53, 2 answers read, 43 recvmsg calls answered EAGAIN. Every UDP
reader took frames off the single legacy queue and dropped what was not its
own; chrome runs its lookups on several UDP sockets at once, so socket A's poll
threw away the answers for B and C. Commit 6065f77: the RX demux sorts UDP
datagrams for a registered local port into that port's queue (as PORTQ does for
TCP), recv/recv_nowait read their own queue first, and sock_readable reports a
queued datagram instead of "never ready". `[udpq]` lines count the routed
datagrams. Run 36 verifies (udp read count, no NAME_NOT_RESOLVED on the page).

### W8. lost timer ticks: the guest clock ran at a third of real time

Run 35, same build as run 34, failed on timing alone: at 325 s the desktop
clock read 1m44s and the profile dialog was still up (the click and Enter had
gone in at 120 s and 200 s real, too early in guest time); the live navigate
came at heartbeat 10 = tick 30000 = 660 s real. Three NMI samples all found the
vCPU halted in the desktop loop's idle (rip after `yield_now`'s int 0x4a and
the hlt), the host sampler idle, the timer calibrated normally. The periodic
LAPIC timer keeps one pending bit per vector: every period spent with
interrupts off beyond the first is lost, and syscalls run with IF=0 (FMASK).
Commit 6065f77: the calibration measures TSC cycles per period, schedule_tick
adds the missed periods to TICKS (the clock stays on real time) and logs the
first late ticks with the interrupted rip, task, last syscall and the
demand-fault counters: `[tick-late]`. Run 36 said which window it is:
`net::pump_all+0x84` in task 0, 0.2 to 1.2 s per window, 54150 ticks (540 s)
lost over the run. pump_all, added with the run-26 freeze fix, called pump(4)
on every open connection under the IfOffGuard, and pump's poll_seg spins 12
million iterations per idle connection. Commit 55b1e58: pump_nowait per pass,
the retransmit pass (tick) once a second. The remaining late ticks are a fork
(+4 ticks, the 256 MiB arena copy with interrupts off) and an execve (+2).

### W9. the GTK crash is not fully gone

Run 36 died again at `_gtk_css_value_ref` (non-canonical pointer, tick 23796,
after a madvise + mprotect burst) with madvise zeroing in place; runs 33 to 35
were clean. It is the first crashing run in which fork children had exited
before the crash (four on-device-model utilities). Commit 55b1e58 adds the exit
guard: a dead child's keep list also holds every frame the browser main still
maps, and `[exit-guard]` prints how many of the child's frames that concerned
outside the shared lists. Run 37 reported zero for both exits, and run 38 crashed the
same way once more. Reading the value again: PartitionAlloc encodes a freelist
word as ~byteswap(next), so a freed slot's first word is -1 when it is the last
entry and non-canonical otherwise, which is exactly what r13 read in every
crash. GTK read the first word of a freed object as a GtkCssValue pointer: a
use-after-free in GTK or chromium's GTK layer, not a kernel memory bug. Commit
6bb4a3b: chrome's desktop argv carries `--ui-toolkit=qt`; the Qt shim finds no
Qt in the pack and the browser runs without a toolkit integration, as it does
on any system without GTK. Documented as a workaround, not a fix.

### W6 continued: the lock is forced open and the holder named

Run 38 wedged in `serial::_print` again, on the boot CR3: the holder was gone,
a task that died or slept while printing, so the same-cpu bypass did not apply.
Commit 6bb4a3b: `_print` waits at most 200 million spins, forces the lock open,
writes the holder's task and cpu (recorded at lock time) through a second port
handle, and goes on. Run 39 then wedged in a print nested inside the thread
census: the census prints with SCHED held, and the new holder record called
sched::current(), which takes SCHED (sched.rs warns about exactly this). The
record is lock-free now (c6e3fa8). Run 39 also mapped libgtk-3 despite
`--ui-toolkit=qt` and died the same way, so openat refuses libgtk-3.so.0 and
libgtk-4.so.1 to the chrome process (argv[0] `/pack/chrome`, 0098985); run 41
is the first with GTK really out.

### W6. the flaky wedge, named: a print nested on the cpu that holds the UART lock

Run 37 went silent at 310 s, ten seconds after the live navigate. The runbook's
QMP dump (added for this) showed the vCPU running, not halted, CPL 0, IF=0, at
`serial::_print+0x2a` (the UART lock acquire) on the https renderer's CR3, and
the NMI probe printed nothing: it needs the same lock. The lock is taken with
interrupts off, which rules out preemption but not re-entrancy on the same cpu:
a page fault while a line is being formatted (an argument that reads user
memory; CR2 pointed into the demand region), or the NMI itself, runs a handler
that prints, and that print spins on a lock its own cpu holds. Commit e6c326e:
`_print` records the holder's lapic id; a print that finds its own cpu recorded
writes past the lock through a second port handle and skips the kmsg tee. The
exit guard of run 37 reported 0 for both child exits: the ownership model of
the demand pages is sound. Run 38 verifies.

### FIRST PASS (run 41, build 0098985) and W6 closed

Run 41: VERDICT PASS. GTK refused at openat and never mapped, no fault on the
browser main thread, the live site committed and loaded in 4.6 s (run 34 took
27 s; the resolver's answers now reach their sockets), no name unresolved, exit
guard 0, 461 ticks lost over the run. The serial watchdog fired twice, both
during the typed keystrokes, and its message closed W6 for good: "UART lock
held by task 0, no printer recorded". `serial::read_byte`, polled by the
desktop loop with interrupts enabled, held the lock through try_lock while the
xHCI interrupt handler printed a keyboard report; that print spun with
interrupts off and the interrupted task could never release. Every wedge
"right after the first keystrokes" (runs 3, 12, 20, 28, 29) was this. Commit
ff31b85: read_byte and write_raw hold the lock under without_interrupts. The
three-run series for exit criterion 3 runs on that build: 42, 43, 44.

Run 42 (ff31b85): PASS, serial watchdog silent, 58 ticks lost over the run.
Run 43 (ff31b85): PASS, 55 ticks lost, live page loaded at 304.7 s.
Run 44 (ff31b85): PASS, 51 ticks lost, live page loaded at 304.4 s.

### EXIT CRITERION 3 MET (runs 42, 43, 44 on ff31b85)

Three consecutive PASS verdicts on one build: no fault on the browser main
thread, the live site committed and loaded within five seconds of the
navigate, every name resolved, no arena refused, no wedge, about 55 ticks lost
per 660 s run (all at the forks and execs). Proof of the third:
`docs/proof/2026-09-24-desktop-chromium-criterion3-run44.png`. The sprint's
three exit criteria are met.

Open after the sprint, in order: the Simple Cache's kBadFakeIndexFile and the
profile-error dialog (W2; `[fsdiag]` in 6feec9d names the syscall), the
handshake with the second host on the same server (tracera.eu,
ERR_SSL_PROTOCOL_ERROR), the GTK use-after-free (W9, worked around), the fork's
256 MiB arena copy with interrupts off (+4 ticks per fork), and the cosmetic
"unsupported command-line flag" bar.
Open, not blocking: the page's one third-party script, `https://tracera.eu/t.js`,
now fails with net::ERR_SSL_PROTOCOL_ERROR (its name resolves since W5b; the
handshake with that host does not complete). The next network item after the
series: capture that handshake server-side or against a local TLS server.

### After the sprint: W2, the profile dialog, chased with fsdiag

Run 45 (6feec9d, fsdiag + sendmmsg/recvmmsg): PASS; no file operation on
/tmp/cr failed at all, which pointed at the open itself: a missing file came
back as -1 (EPERM). The Simple Cache only writes a new fake index on ENOENT, so
every run since the first had "wrong file structure on disk: 2"
(kBadFakeIndexFile) and no disk cache. Commit 1837891: openat answers ENOENT
or EMFILE; chmod/fchmod/fchmodat and sigaltstack answer 0.

Run 46 (1837891): PASS; the cache creates `Cache/Cache_Data/index` and the
Shared Dictionary index and runs; ENOSYS left: name_to_handle_at, landlock.
The dialog is still up at 120 s, with "Could not open the quota database" and
"Failed to load tokens (invalid SQL statement)" (Web Data's token table).
Both are SQLite, and fcntl answered every lock command with 0 without writing
the struct: unixCheckReservedLock reads its own F_WRLCK back and takes the
database as locked. Commit (F_GETLK writes F_UNLCK): run 47 verifies.

The third-party script on the live page (tracera.eu, served by Caddy on
127.0.0.1:9443 behind the SNI router) failed with ERR_SSL_PROTOCOL_ERROR in
runs 42 and 44 and loaded in 41, 43 and 46: intermittent, TCP drop counters
all zero in run 46. Still open.

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
2. Same run against `https://euro-os.eu/` renders the site. MET, run 34.
3. Three consecutive runs pass (the repeatability bar used for multi-process).
   MET: runs 42, 43, 44 on build ff31b85 (run 41 was the first PASS).
