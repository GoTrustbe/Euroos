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
