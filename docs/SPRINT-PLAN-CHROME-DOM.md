# Sprint: chrome-headless-shell → DOM (getrandom-area wall and beyond)

## Goal
Advance `chrome-headless-shell --dump-dom file:///tmp/euro.html` past its current
IMMEDIATE_CRASH toward emitting `<h1>EuroOS</h1>`. Each wall is a real Linux-ABI
gap; fixing it is generally valuable, not chrome-specific.

## Where we are (2026-08-10)
Three walls cleared this session (thread-pool → FPU save; Mojo channel_linux.cc:926
→ memfd flags; fcntl access-mode → dup accmode). Crash now at **0x335d4a6**,
last-syscall **getrandom(318)=16**, in an XML/cxxbridge region.

## Method (proven this session)
1. Boot hs-pack (`-m 2048M`, ~8 min build+boot); read the `[idt] ring-3 GP FAULT
   ... IMMEDIATE_CRASH | last-syscall=N` line (RIP + last syscall + fd_kind).
2. Map RIP→file offset: `off = RIP - 0x100_0000_0000` (DEMAND_BASE); objdump uses
   vaddr directly on `/tmp/hs-bin` (LOAD2 bias 0x1000).
3. `objdump -d --start-address=<RIP-0x180> --stop-address=<RIP+8> /tmp/hs-bin`;
   find the `jXX <crash>` that branches to RIP → read the CHECK it guards.
4. Fix the kernel ABI so the CHECK passes; rebuild; boot; confirm the crash MOVED.

## Backlog (in order)
- S1 — Diagnose 0x335d4a6: disasm the branch to it; identify the CHECK + the
  syscall/state it depends on (getrandom flags? a parse result? a fd?).
- S2 — Fix the identified ABI gap; verify the crash moves forward.
- S3 — Repeat the loop on the next crash until navigation starts (watch for
  `euro.html` access / DOM output) or a structurally different wall (GPU/multiproc).
- S4 — If a wall needs the renderer process (execve/M2), stop and re-scope; the
  single-process headless path should reach the DOM without it (it is what
  --dump-dom is designed for).

## Guardrails
- Keep every fix a correct, general ABI improvement; never special-case chrome.
- Commit each cleared wall separately (no Claude trailers; do not push).
- Permanent diagnostics (last_syscall/fd_kind/#GP insn decode) stay in.

## Iteration 1 result (2026-08-10): WALL 4 CLEARED
S1 diagnosed 0x335d4a6 = a memcmp tree-walk ending in `cmp expected_ptr; jne crash`
(a map/tree lookup CHECK), right after getrandom(318). Root cause: our getrandom
filled from byte POSITION only → every call returned identical bytes → "random"
IDs/tokens collided → a map had 1 entry where 2 were expected → lookup CHECK
crashed. S2 fix (commit c511ab0): getrandom now unique-per-call (splitmix64 over a
fetch_add counter). VERIFIED: IMMEDIATE_CRASH count 0; chrome advances past it into
resource loading (file-backed .pak mmaps).

Wall chain now: thread-pool → Mojo → fcntl-accmode → getrandom-uniqueness (4 cleared).

## Next iteration target
Chrome now forks (headless-shell forks ~3 children; 1 succeeds, 2 fail
"[fork] arena alloc FAILED (96 MiB, pool has 55 MiB)"). The forked child does NOT
execve (so NOT the M2 wall). Boot then stalls during resource loading (log stops at
a successful disk-backed mmap; no crash, no fatal). Two threads to pull:
- FORK POOL DEPLETION: the 640 MiB procpool drops to 55 MiB after one 96 MiB fork —
  clone_demand_region (paging.rs:570) likely copies committed demand pages from
  procpool; a process with a big committed demand region depletes it. Fix: copy into
  the DEMAND pool (or COW) so forks don't exhaust procpool; and/or free child arenas.
- THE STALL: determine if chrome is hung (waiting on the failed-fork child / disk
  cache) or just slow. Add progress instrumentation or a longer boot. Disk-cache
  "Unable to create cache" persists (our VFS cache-dir ops) — likely non-fatal but
  worth ruling out.

## Iteration 3 result (2026-08-11): the stall is an EPOLL LIVELOCK on a forked child
Enabled STALL_DIAG for the hshell run. Verdict: NOT a hang, NOT slow — a LIVELOCK.
Snapshots (steady across 6 windows): ~5.5M syscalls + ~780K epoll_wait per window,
~0 futex, task states frozen at "10 Ready, 12 Blocked, 4 Sleeping (of 45); current=0",
threads=16, ticks advancing. So ~10 chrome threads spin in epoll_wait forever; our
epoll_wait already yields (8 tries + sleep + yield) — the threads are waiting for an
fd event that never arrives.

ROOT: chrome forks a helper child but it FAILS ("[fork] arena alloc FAILED (96 MiB,
pool has 55 MiB)"); chrome then blocks (epoll) on the child's IPC socket that never
connects because the child was never created. This is the fundamental MULTI-PROCESS
wall: even single-process chrome-headless-shell forks helper processes and waits on
their IPC. Reaching the DOM from here needs the forks to SUCCEED (fix the procpool
depletion: 640 MiB -> 55 MiB — investigate what holds it; the successful fork only
took 96 MiB) AND the child to run its helper role (fork-only, or execve = M2).

## Session verdict
Cleared 4 deliberate-CHECK crash walls (thread-pool, Mojo, fcntl-accmode, getrandom-
uniqueness) — chrome-headless-shell now runs its full startup into the event loop,
far past any prior point. The remaining wall is structural (multi-process fork+IPC),
the same rock flagged from the start. Next real work = M2 (make chrome's forks a
first-class success, incl. per-process state) — a multi-day project, not a syscall
patch. All 4 wall fixes are general Linux-ABI correctness wins independent of chrome.

## Iteration 4 (2026-08-11): forks succeed at -m 4096M, but LIVELOCK PERSISTS
The procpool "depletion" was just sizing: it scales to ~1/5 RAM, so -m 2048M gives a
160 MiB pool (one 96 MiB fork fits, the second OOMs); -m 4096M gives 640 MiB. At 4 GB
all 3 chrome forks SUCCEED (tasks 42/44/47). BUT the epoll livelock is UNCHANGED
(~535K epoll/window, 12 Ready spinning / 13 Blocked). So fork success is not enough:
the forked children NEVER execve (no [spawndiag] execve) into their helper process
types, so chrome's main process keeps blocking on their IPC sockets forever.

CONCLUSION (definitive): the DOM wall is the MULTI-PROCESS MODEL — chrome forks
helper children that must execve into --type=utility/... processes with their own
per-process state and connect back over Mojo IPC. Our fork creates the child task
but it does not become a functional helper (execve = M2 is ENOSYS on the demand-
paged model; the child also shares the parent's singleton globals = M3). This is the
same structural rock flagged from the start, now reached with certainty AFTER
clearing every deliberate-CHECK crash. Getting the DOM = the M2/M3 multi-process
project (multi-day), not a syscall patch.

## BREAKTHROUGH 2026-08-11: --single-process sidesteps the multi-process wall
Switched chrome-headless-shell to --single-process --in-process-gpu. This is the
strategically right path: it avoids fork+execve+cross-process-Mojo (the multi-day
M2/M3 refactor) entirely — the renderer/GPU/utility all run in the browser process.
It was "upstream-broken" before only because of the worker-thread CHECK crashes we'd
been clearing. Results (iterations, each a real ABI fix that moved the crash forward):
- forks: 0 (multi-process wall GONE), epoll rate collapsed 780K->8K/window.
- prlimit64/getrlimit now fill the rlimit buffer (were returning success unfilled ->
  garbage limits -> CHECK crash). Moved crash fontations -> pthread.
- alloc_thread_kstack self-heals leaked slots (fault-killed threads never freed them).
- glibc thread stacks (MAP_STACK ~8MiB) route to the 256GiB demand region, not the
  ~31MiB arena mmap window that a few threads exhausted -> pthread_create EAGAIN gone.
  Thread count 2 -> 22: single-process chrome now runs its full thread set.

CURRENT WALL: Fontations (chrome's Rust font lib, variation_position) crashes at a
getrlimit64(6, computed_ptr) probe that expects -1/EFAULT. This is an error-handler/
guard-probe path (only sensible if reached after a prior fault — likely a stack-guard
or bounds probe). Our getrlimit returns success because the probe pointer passes
in_user_arena. Root not yet isolated: is fontations reaching this because of a REAL
prior fault (e.g. a thread-stack guard-page issue from the demand-region stacks), or
does getrlimit need to EFAULT here? Needs memory-model investigation, not a syscall
patch. This is the single remaining --single-process blocker.

## Session verdict (updated)
7 walls cleared; the strategic picture flipped: the DOM no longer requires the
multi-process refactor — --single-process is viable and chrome now runs 22 threads
deep into font init. One subtle Fontations guard-probe crash remains before Blink.

## Wall 9 CLEARED 2026-08-11: read-only mapping enforcement — chrome runs clean
Diagnosed via a temp prlimit-buffer log: fontations' probe is getrlimit(6, ptr)
expecting EFAULT where ptr is a file-backed (read-only) demand mapping (a font/lib
segment). Our RW-everything model returned success -> crash. Fix (commit): honor
PROT_NONE (mprotect/mmap prot==0 -> in_user_arena/handle_demand_fault reject) AND
enforce read-only file-backed mappings (write_user/copy_to_user EFAULT inside a
DEMAND_FILE_MAPS range). VERIFIED: IMMEDIATE_CRASH count 0 (was 1 every run);
single-process chrome now runs 24 threads with ZERO crashes, fully initialized.

## Milestone: single-process chrome runs crash-free into init; navigation not started
9 walls cleared this session. chrome-headless-shell --single-process now:
- 24 threads, no crashes, no forks.
- Services init (inotify/netlink/dbus/udev fail gracefully; audio -> ALSA fallback).
- Thread pool IDLE: ~14 workers Blocked on futex (normal idle pool), a few threads
  Ready epoll-waiting.
- Has NOT navigated to file:///tmp/euro.html -> no DOM yet.
This is now a DIFFERENT class of wall: "navigation never starts" (a coordination/
service dependency), not an IMMEDIATE_CRASH. Two threads to pull:
1. Why no navigation? headless --dump-dom should auto-navigate. Something the active
   threads epoll-wait on isn't completing (a service? the compositor? disk cache?).
2. The boot selftest spinners (tasks 7-11: counter tasks + tlscount, infinite loops
   in the selftest block) run Ready alongside chrome and STEAL CPU under the
   cooperative scheduler — gate them off for the chrome test so chrome gets the core.
NEXT: instrument which fd/event the active chrome threads wait on; consider disabling
the disk cache / more services; give chrome the CPU (gate the spinners).

## THE WALL BEFORE BLINK 2026-08-11: multi-threaded futex deadlock
With the CPU freed (spinners gated) + navigation forced (--virtual-time-budget) +
demand region enlarged (256->480 GiB), single-process chrome now runs 4000+ log lines
deep into V8 GigaCage/PartitionAlloc setup — the furthest ever — then DEADLOCKS:
STALL_DIAG shows +0 syscalls / +0 futex / +0 epoll across snapshots and "TIMER DEAD";
all ~23 threads Blocked, nothing runnable. Confirmed NOT memory (480 GiB still ENOMEMs
but the deadlock is identical). This is a genuine lost futex wake / circular block
under the cooperative scheduler — the "real wall before Blink" flagged from the start.
NEXT (dedicated effort): instrument futex_wait(addr,task) + futex_wake(addr,count),
find the wait with no matching wake (or the cycle); likely a wake issued before the
wait registered (lost wakeup) or a wake that doesn't hash to the waiter's channel.
This is a concurrency-correctness project, not a syscall patch.

## Answer: does chrome "work"?
It RUNS — single-process, ~23 threads, zero crashes, deep into V8 init (9 CHECK-crash
walls cleared this session). It does NOT yet render/emit a DOM: it deadlocks on a lost
futex wake before navigation completes. Runs clean, no output yet.

## MILESTONE 2026-08-11: the deadlock is BROKEN (deadlock -> livelock)
Root cause of the wall-before-Blink: chrome does TIMED futex waits (FUTEX_WAIT_BITSET
op9 + absolute timeout; confirmed via diag) and our futex_wait IGNORED the timeout ->
blocked forever. Fixed (commits): honor futex timeouts (park as Sleeping(deadline),
auto-wake, -ETIMEDOUT; futex_wake -> unblock_any) + tickless idle (run_glibc_disk
fast-forwards TICKS to the soonest deadline instead of busy-spinning, so multi-second
timed waits elapse in wall time under TCG). RESULT: deadlock GONE — guest time races
(+4000 ticks/snap vs +1), futex/epoll activity high (+11000 syscalls/+8000 epoll vs
+0), all 24 threads active.

NEW state: chrome no longer deadlocks but LIVELOCKS — busy-polls epoll steadily with
no forward progress (identical ~13050 demand pages at 60000 AND 180000 ticks; no
navigation). It inits subsystems (audio, vaapi GPU) then spins. This is "navigation
never starts": the threads wait on an fd/event that never fires. Teardown side-note:
3 clock_gettime EFAULT crashes AFTER the run times out (leftover threads run with
DEMAND_ENABLED already cleared) — cosmetic, not the blocker.
NEXT: identify the fd/event the busy threads epoll-wait on (instrument epoll_wait
targets); reconsider flags (--run-all-compositor-stages / --in-process-gpu may make
navigation wait on a compositor frame that never commits without GL).

## Answer: does chrome work? (updated)
RUNS actively, single-process, 24 threads, 10 walls cleared incl. the deadlock. Does
NOT render/emit a DOM: it now LIVELOCKS at "navigation never starts" instead of
deadlocking. Closer than ever; the remaining wall is chrome-init coordination, not a
crash or a deadlock.

## MILESTONE 2 2026-08-11: navigation livelock BROKEN (poll pipe-readiness)
After the deadlock fix, chrome livelocked: the MAIN thread spun in poll() returning
1-ready forever (traced via per-thread last-syscall dump: MAIN task = poll, ->1). ROOT:
poll() classified a PIPE as a 'regular' fd -> reported always-ready, so chrome's
message-pump wakeup pipe looked perpetually readable and the pump spun (navigation
never started). Fix (commit): poll() now uses the same readiness as epoll
(epoll_fd_ready/epoll_fd_writable) — event fds/pipes/sockets report POLLIN only with
real data; only std streams + regular files stay always-ready.

RESULT: livelock GONE. chrome progresses far deeper — past the pump, through Shared
Dictionary cache, into pango/fontconfig init; the 3 teardown crashes disappeared.
NEW WALL: GLib g_error creating the '[pango] fontconfig' thread — pthread_create
EAGAIN. NOT the kstack pool (thread high-water only 31 << 224) nor spawn_thread MAX;
instrumentation of clone3 didn't fire for the failing thread (path varies run-to-run).
Suspects: glibc thread-stack mmap of a huge/corrupt size (V8/GLib reserve >480 GiB;
1.4 TB + 553 GiB anon RESERVE ENOMEM in the log), or a thread stack routed to the
small ~96 MiB arena instead of the demand region. NEXT: log the stack mmap size/route
for the failing thread; ensure ALL thread stacks (any size, MAP_STACK or not) go to
the demand region; consider spanning the demand region across >1 PML4 slot for V8's
>512 GiB reservations.

## Session status (11 walls)
chrome-headless-shell --single-process now runs with NO deadlock and NO livelock,
deep into font/pango init — dramatically further than ever. DOM not yet emitted;
next wall is a pango thread-creation EAGAIN. Deadlock + livelock were the two
structural walls; both are broken.

## Wall 12 (open) 2026-08-11: pango thread EAGAIN — arena-size-entangled
After the livelock broke, chrome reaches pango/fontconfig init and GLib g_errors
creating the '[pango] fontconfig' thread (pthread_create EAGAIN). Findings:
- NOT the kstack pool (thread hw 31 << 224), NOT spawn_thread MAX, NOT a MAP_STACK
  demand mmap ENOMEM (all instrumented, none fired). clone3 instrumentation didn't
  fire for the failing thread (nondeterministic path).
- A 384 MiB arena (vs 96) FIXES the EAGAIN — but has a SEPARATE startup bug: chrome
  commits 0 demand pages and never runs, even with a 1.8 GiB pool (-m 5120M). So the
  384 MiB arena breaks ld.so entry/layout, independent of pool size.
- Routing ALL anon+file mmaps to the demand region (sparing the arena's ~30 MiB mmap
  window) did NOT fix the EAGAIN -> it is NOT arena mmap-window exhaustion. The cause
  is arena-size-dependent in another way (main-thread stack region? a glibc stack-size
  computation?). Reverted mmap thresholds + arena to the working 96 MiB.
NEXT (dedicated): (a) find WHY a 384 MiB arena yields 0 committed pages / no ld.so
run (loader stack/entry layout vs arena size); fixing that makes the 384 MiB arena
usable and clears pango. (b) OR instrument glibc's failing pthread path directly
(log every mmap ENOMEM regardless of MAP_STACK, and clone (56) EAGAIN, and the
requested stack size) to pin the EAGAIN source.

## Session status: two structural walls broken, one resource wall open
11 walls cleared incl. the deadlock and the navigation livelock (the two structural
blockers). chrome-headless-shell --single-process runs crash-free, no deadlock, no
livelock, deep into font/pango init. DOM not yet emitted; the open wall is the pango
thread EAGAIN (arena-size-entangled, needs dedicated debugging, not resource tuning).

## Wall 13 CLEARED 2026-08-11: MAP_SHARED was a private copy (the empty-document wall)
After the dir-enumeration fixes, chrome ran further than ever and then exited
CLEANLY with code 0 and no DOM, in the middle of storage init. Two diagnostics
settled what "cleanly" meant:
- `[exitgrp]`: the MAIN task called exit_group(0), and the user-stack return
  addresses were the _start/__libc_start_main/main frames. So chrome's main()
  RETURNED normally: the browser main loop had simply ended. Not a crash, not a
  timeout (the host still dumps a DOM with --timeout=1, so the shell timeout is
  not the mechanism).
- `--vmodule=simple_devtools_protocol_client=2` (the host oracle proves --dump-dom
  is driven entirely over CDP): EuroOS produced the FULL round-trip —
  Target.exposeDevToolsProtocol -> RECV, Inspector.enable, Runtime.evaluate of
  `executeCommands(...)` -> RECV. **V8 runs JS on EuroOS.** The answer was
  `ReferenceError: executeCommands is not defined`.

That function is defined by chrome://headless/headless_command.html, the handler
page whose resources come from headless_command_resources.pak. `[packpath]`
tracing proved the pak was found (access -> 0), opened (fd 37) and read (3033 B).
So the resource was present and the page was still empty.

ROOT CAUSE: our file-backed mmap ALWAYS made a private copy — MAP_SHARED did not
exist. Mojo moves every resource body through a memfd ring buffer that producer
and consumer map separately (even in one process), so the reader saw zeros: an
empty document, no error anywhere. Fix (commit 917c60b): a MAP_SHARED mapping of
an in-RAM file maps the whole file into ONE arena region, and every later mapping
resolves to that region; read()/write() reconcile at the fd boundary. Verified by
a new test gshm (16/16 pages mismatched before, PASS after) — LINUX COMPAT 21/21.

Method note: the host oracle (same binary, same flags, native Linux) was decisive
twice — it named the CDP mechanism and it ruled out the shell timeout.

## VERIFIED 2026-08-11: NAVIGATION WORKS — the gap is only the chrome://headless page
Ran chrome-headless-shell with a BARE URL (no --dump-dom, no --timeout: the host
oracle proves BOTH put chrome in command-handler mode, so neither tests plain
navigation). Result on EuroOS:

    [hshell] BUILD=bare-url-navigation
    CDP=0  FILEURL=2
    VERBOSE1:content/browser/loader/file_url_loader_factory.cc:474]
        FileURLLoader::Start: file:///tmp/euro.html

So chrome NAVIGATES on EuroOS and its loader starts reading the page. Navigation,
the URL loader and Mojo resource plumbing all work. The single remaining gap is
that `chrome://headless/headless_command.html` (the WebUI page that defines the
`executeCommands` JS which --dump-dom evaluates) comes up EMPTY — silently: with
output capture made lossy (a write with invalid UTF-8 used to be dropped WHOLE,
which could have hidden a message) chrome still reports NOTHING about the pak. It
finds it (access -> 0), opens it (fd 37) and reads all 3033 bytes (verified
byte-identical to the real file inside the pack image).

Status of the DOM: chrome runs, navigates, loads the page and executes JS; the
only thing missing is the injected command JS. Next step = drive CDP OURSELVES
via `--remote-debugging-pipe` (fd 3 in / fd 4 out, JSON messages NUL-separated):
the kernel sends Page.navigate + Runtime.evaluate("document.documentElement
.outerHTML") and reads the DOM back. That bypasses the WebUI page entirely and
uses only the two things now proven to work: navigation and V8.

## ★★★ MILESTONE 2026-08-11: CHROMIUM RENDERS A REAL PAGE ON EUROOS
    [cdp] readyState=complete DOM (1119 B): <html><head><meta charset="utf-8">
          <title>Chromium on EuroOS</title>... (the full styled page)
    [cdp] ★★★ REAL DOM rendered by Chromium on EuroOS
chrome-headless-shell loads file:///tmp/euro.html, Blink parses it, and the kernel
reads the document back over DevTools. 0 IMMEDIATE_CRASH, chrome exits 0.

### How it was found: EuroOS speaks DevTools itself
--dump-dom never navigates by itself: it loads chrome://headless/headless_command
.html (a WebUI page) and evaluates its executeCommands JS. That page came up EMPTY,
so nothing observable happened. Instead of chasing the WebUI, the kernel now drives
the protocol directly over --remote-debugging-pipe (fd 3 in, fd 4 out,
NUL-separated JSON; ring3::cdp_install/cdp_pump): Target.getTargets ->
Target.attachToTarget(flatten) -> Page.enable -> Page.navigate ->
Runtime.evaluate("document.readyState+'|'+document.documentElement.outerHTML").
The sequence was validated on native Linux first (/tmp/cdp_drive.py) so the guest
was never debugged against a guessed protocol. Reading readyState ALONGSIDE the
markup is what separated "read too early" from "the body never arrived".

### The three bugs (each: an empty document, no error anywhere)
1. **mmap's fd argument was read as 64 bits.** It is an INT. Chrome arrives with
   0xffffffff_00000033 for fd 51, so a MAP_SHARED mapping of a real file looked
   ANONYMOUS: its shared buffers became private zero pages and the page bytes never
   reached the renderer. Found with a bounded syscall trace armed the moment chrome
   sized a shared buffer (SYS_TRACE_LEFT) — the fix is one truncation.
2. **unlink() shifted every FILES index** while open fds hold exactly such an index,
   so "create, unlink, ftruncate, mmap" (anonymous shared memory, and how chrome
   allocates Mojo buffers) handed descriptors someone else's data. Now tombstoned.
3. **MAP_SHARED did not exist** (every mmap was a private copy). A first fix that
   returned ONE address per file made chrome CHECK-fail — it registers mappings BY
   ADDRESS. Shared mappings now each get their own address range and fault onto the
   file's shared frames: one memory, distinct addresses.

Tests: gshm (two MAP_SHARED mappings of one memfd are one memory; 16/16 pages
mismatched before) and gunlink (unlinked fd alive, neighbours intact, anon shm
shared). Both fail hard on the old kernel.

### Next
- Page.captureScreenshot over the same pipe: real Blink-rendered pixels (software
  raster, no GL — SwiftShader needs AVX2 that qemu64 lacks). Heavy under TCG.
- Re-test --dump-dom: the WebUI page may well load now that shared memory works.

## ★★★ CONFIRMED: chrome's OWN --dump-dom works (no kernel driver involved)
With the shared-memory bugs fixed, the chrome://headless handler page loads, its
JS runs, and unmodified chrome-headless-shell prints the document itself:

    [hshell]   <html><head><meta charset="utf-8"><title>Chromium on EuroOS</title>
    ...
    [hshell]   <div class="card">Shared memory carries the page bytes to the renderer</div>
    [hshell]   </body></html>
    [hshell] chrome-headless-shell from DISK: exit=0     (0 IMMEDIATE_CRASH)

So the empty WebUI page had exactly the same root cause as the empty file:// body:
mmap's fd argument read as 64 bits, unlink shifting FILES indices, and no
MAP_SHARED. The kernel's own DevTools driver (--remote-debugging-pipe) remains a
real capability and is what made the failure observable in the first place, but
chrome's stock feature no longer needs it. NOTE: the two are mutually exclusive —
chrome refuses "Headless commands ... with remote debugging" — so they are tested
in separate runs.

Pixels remain open: Page.captureScreenshot never answers (chrome spins on a
compositor frame that never commits without GL; SwiftShader needs AVX2 the CPU
lacks). That is software frame production in Viz, a different wall from the DOM.

## Pixels, wall 14: poll() ignored its timeout (fixed) — frame production still open
Chasing Page.captureScreenshot, a bounded syscall trace armed at the request showed
one thread calling `poll(fds, 2, -1)` millions of times per second, always answered
0. With an INFINITE timeout, 0 is a lie ("your timeout expired"), so the caller
loops straight back in and starves the threads that would produce the frame. Our
poll() ignored the timeout entirely. Fixed (commit 869c16c): poll now re-checks,
gives the CPU up between checks, and reports 0 only when a finite timeout really
elapsed. New test gpoll passes with identical numbers on EuroOS and native Linux.
Effect: chrome's syscall storm halves; its threads WAIT (epoll/futex) instead of
spinning in poll.

STILL NO PNG, even with a 300 s guest-time budget. So this is no longer a spin:
the compositor never commits a frame. IMPORTANT correction to the earlier note:
the flags are NOT the cause — native Linux with this exact flag set
(--use-gl=disabled --disable-gpu-compositing --ozone-platform=headless) returns a
5138-byte PNG for both fromSurface=true and false. Software rasterizing works
there without SwiftShader, so "SwiftShader needs AVX2" does not explain this.

NEXT for pixels: find what the frame is waiting on. Ideas, cheapest first:
- vmodule the compositor/viz side and compare against the host run line by line;
- check whether a BeginFrame is ever requested (headless uses a synthetic
  BeginFrameSource — if its timer never ticks for us, nothing draws);
- try --run-all-compositor-stages-before-draw (+ virtual time), the combination
  headless screenshots historically needed;
- watch for a Mojo reply that never comes on the Viz interface.

## Pixels: the same wall from both directions (2026-08-12)
Tried chrome's OWN `--screenshot=/tmp/euroshot.png` instead of driving
Page.captureScreenshot ourselves — the same command handler that makes --dump-dom
work here. Result: chrome exits 0 and writes NO file, and the DOM is not printed
either, because executeCommands awaits the capture that never completes. So the
capture is the blocker, not the way it is requested.

Two independent routes, one wall: no compositor frame is ever produced. Everything
around it works (navigation, resource loading, V8, the DOM). Reverted to
--dump-dom alone: the document should not wait on pixels.

Note the compositor/viz modules emit no VLOG output even at --v=2 on the host, so
log-diffing will not locate this; it needs a different probe (is a BeginFrame ever
requested? does the synthetic BeginFrameSource tick? does a Viz Mojo reply come
back?).

## Pixels: third route also fails — it is frame production, full stop (2026-08-12)
Tried the combination headless screenshots are documented to need, now that
navigation works: `--run-all-compositor-stages-before-draw --virtual-time-budget=8000
--screenshot=`. Same outcome: chrome exits 0, no PNG, and no DOM either (the
capture blocks executeCommands).

Three independent routes, one wall:
  1. Page.captureScreenshot over our own pipe (fromSurface true AND false),
  2. chrome's own --screenshot=,
  3. the two flags above on top of it.
Everything around a frame works — navigation, resource loading, V8, the DOM, and
threads now WAIT properly (after the poll-timeout fix) instead of spinning. So this
is not a flag or a request-shape problem: no compositor frame is ever produced.

Config left at --dump-dom alone (the working result should not wait on the missing
one). A real investigation needs a probe into whether a BeginFrame is ever
requested and whether the synthetic BeginFrameSource ticks here — viz/compositor
emit no VLOG even at --v=2, so logs will not show it.

## Pixels: real sleeps did not unblock it either (2026-08-12)
nanosleep was a no-op and clock_nanosleep unimplemented on the glibc path — the
same class of bug as poll's ignored timeout, and a real reason to expect a
different outcome, since a compositor paces frames on deadlines. Both are fixed
(commit 229a538, test gsleep: 60 ms takes 60 ms, an absolute +80 ms deadline waits
80 ms, matching native Linux exactly). Chrome still writes no PNG.

Four ways of asking now end identically, so the request is not the problem:
  1. Page.captureScreenshot over our pipe (fromSurface true AND false),
  2. chrome's own --screenshot=,
  3. + --run-all-compositor-stages-before-draw + --virtual-time-budget,
  4. all of the above with working nanosleep/clock_nanosleep.

LEAD for next time (untested): in headless software compositing the frame lands in
a SoftwareOutputDevice backed by a SHARED BITMAP. Large shared mappings now go
through the demand-region aliasing path (own address per mapping, shared frames) —
worth checking whether the display side ever sees what the renderer wrote there,
e.g. by having a test map a >1 MiB shared file from two mappings and comparing
(gshm only covers a 64 KiB region, which takes the ARENA path, not the aliasing
one). If that gap is real it would explain a frame that is never "ready".

## The shared-bitmap lead is CLOSED (2026-08-12)
Extended gshm to the path a browser actually uses: demand paging on, a 4 MiB
shared region, 1024 pages checked one way and the far end back. It passes on
EuroOS exactly as on native Linux. On the way it found and fixed a real gap
(read()/pread() did not see writes made through an ALIASED shared mapping; the
reconciliation now goes through the shared frames via the identity map).

So shared memory holds up at browser scale, and the missing compositor frame is
NOT explained by it. Remaining ideas for whoever picks this up: is a BeginFrame
ever requested at all, and does the synthetic BeginFrameSource tick here? Neither
is visible in logs (viz/compositor emit no VLOG even at --v=2), so it needs a
probe in the kernel (which timer/fd does the compositor thread wait on, and does
that wait ever complete?).

## Pixels: the threads are IDLE, no frame is ever REQUESTED (2026-08-12)
A bounded wait-diagnosis (WAIT_DIAG, armed the moment we ask for a capture; it
describes every unsatisfied poll/epoll: which fds, of what kind, and whether any
is ready) changed the picture completely. After the poll and sleep fixes, chrome
is not stuck on a frame — it is IDLE:

    [wait] t10 epoll_wait timeout=53740 nothing ready: fd802(eventfd,in=false,out=true)
    [wait] t22 epoll_wait timeout=73300 nothing ready: fd804(eventfd,in=false,out=true)
    [wait] t7  poll timeout=2850ms nothing ready: fd12(pipe,...) fd800(eventfd,...)

Timeouts of 53-74 SECONDS are idle housekeeping timers. A compositor with a frame
to draw would be waiting ~16 ms. So the capture request never turns into scheduled
work: nothing is requesting a frame at all.

Also tried, and worth knowing:
- `HeadlessExperimental.beginFrame` (the explicit "produce one frame" command) is
  accepted by this build but returns `{}` with no screenshotData — a stub.
- `--enable-begin-frame-control` switches chrome to a mode where NOTHING renders
  unless frames are driven externally: on the HOST it then hangs our driver
  entirely. It is a different rendering contract, not a fix.

So the wall is upstream of the compositor: whatever normally asks for a frame in
this configuration never does so here. Next probe would have to look at Viz/Display
setup rather than at waits — e.g. whether the software Display is ever created.
