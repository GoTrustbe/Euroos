# Sprint 5: the wake that never arrives

## Standing
The machine is cheap now (sprint 4: paint 6 s after map) and input delivery is
proven (sprint 3). What remains: after its paint burst, chrome's main thread grinds
a user-space loop forever — ~20 syscalls/s process-wide, no X requests, no event
collection, 21% of its samples on one libc page consistent with glibc's adaptive
mutex spin. Leading suspect: a lost futex wake — the lock's holder is parked and
the spinner's wake never comes. This project has beaten this class before
(BUG thread-pool deadlock); the tooling from sprint 4 makes it findable now.

## Rule
The trace must NAME the address and the threads: "thread A waits on futex X since
tick T; thread B last woke X at tick U; B is now Blocked on Y" — a chain, not a
vibe. No fix without the chain.

## Phase A — see the main thread's actual syscalls
- A1. A per-main-thread ring of the last 32 syscalls (num, a1, a2, return), dumped
  when the input-unread stall detector arms. If the spin enters futex_wait at all,
  this shows the address and the return codes (EAGAIN? ETIMEDOUT? 0?).
- A2. Futex bookkeeping: per task, the address it currently waits on + since-tick;
  per address, the last waker task + tick. Dumped with the stall dump.

## Phase B — the chain
- B1. From the dump: the futex address the main thread waits/spins on, who else
  touched it, and what state its presumed holder is in.
- B2. If the holder is Sleeping/Blocked forever: why — its own last syscall tells.

## Phase C — the fix, proven
- C1. Fix the identified drop (requeue? wake-op? a wake while the waiter was
  between check and sleep?).
- C2. Proof: the same run collects the click (client reads after the event) and
  the tab-strip screenshot changes. That was the sprint-3 goal all along.

## Also in scope (found in sprint 4, correctness-adjacent)
- The guest clock races 4.3x during startup: dozens of threads each force
  TICKS forward in the epoll/poll retry loops (`TICKS.store(before+1)`). Replace
  the per-thread forced advance with a single monotonic guard so a frozen clock
  still moves but a busy one is not stampeded.

## Results so far (2026-08-22, all measured)

**EuroOS has a working vDSO** (commits 3db3864..41c5294). clock_gettime was 67% of
ALL syscalls in a chrome run. Three walls, each named by its own crash or number:
1. The vDSO pages were promised in the auxv but unmapped — map_demand_4k needs the
   demand pool for its table frames and that pool does not exist at process build.
   ld.so faulted at VDSO_BASE+0x20. Fix: paging::map_user_4k_falloc.
2. An exported data symbol goes through the GOT; nobody relocates a vDSO; the image
   carried exactly one R_X86_64_GLOB_DAT — null deref at vdso+0x1053. Fix: hidden
   `__ehdr_start + 4096`, PC-relative, data page outside the image.
3. glibc rejected the 4-LOAD gcc-default image WHOLESALE (discriminator run:
   gettimeofday and clock_gettime both syscall-bound). The real Linux vDSO is one
   R+X PT_LOAD with FILEHDR+hashes+dynsym+text together; vdso.lds builds that shape
   (0x4e0 bytes, zero relocations).

Proof, guest-side (gvdso probe): 200k glibc clock_gettime calls 1470 ms -> 50 ms;
gettimeofday 950 -> 90 ms. Chrome-scale: **52 684 total syscalls where the same
boot made 182 138** — clock_gettime out of the top five entirely; startup's ~50k
syscalls complete within the first heartbeat interval instead of minutes.

Also learned at cost: the "kernel stack overflow, task 5" chased across three runs
is the G1 self-test overflowing a guarded stack ON PURPOSE at every boot; doubling
stacks made the intentional overflow cross its canary before its guard and panicked
the scheduler. Reverted; read the boot log two lines earlier next time.

## The wall, now with an address

With the clock cheap, chrome races through startup and then PARKS: ring-3 ticks
near zero. The forensics converge on one point: the main thread,
ThreadPoolServiceThread and VizCompositorThread all block FOREVER (27k+ ticks) on
the SAME futex, libc+0x204b50 — glibc's stderr stream lock — each immediately
after writing a multi-KB message to stderr (write(2) returned 0x25ca / 0x24c6,
completed). No chrome thread died holding it. No wait set anywhere contains an X
connection fd, so no click can wake anything.

## Next (phase B, sharpened)
- Dump the LOCK WORD at the waited address in the stall dump (owner/waiters bits).
- Trace every futex op touching that one address, unfiltered, from boot.
- Suspects, in order: a wake swallowed by our futex when the word transitions
  2->0 concurrently; stdio lock elision (_IO_lock) semantics we violate; a writer
  parked inside OUR write(2) path while holding the lock.

## Session close (2026-08-23): what the traps established

- **ab2's lock-word dump split the deadlock three ways in one screen**: two waiters
  parked on words reading 0 (mutex FREE, waiter asleep — a wake demonstrably lost),
  two on the stderr lock reading 2, one on 1. The word dump works and is the tool.
- **The lost-wake trap** (futex_wake finding nobody in the queue while the per-task
  table shows a parked waiter) armed and fired ZERO times in a full run — so the
  queue and table never disagree at wake time. Whatever loses the wake does so on
  the glibc side of the word, or the wake is never sent.
- **Clock honesty has a price.** After unifying TICKS and the vDSO page, guest
  ticks track wall time ~1:1 — and chrome's paint went from "sometimes 6 s" to
  242 s to (trap1) no window in 45 minutes. Hypothesis for next session: with an
  honest fast clock, chrome's own timeouts and backoffs now bind at true TCG
  slowness; the crawling clock used to compress them. The A/B switch
  (EUROOS_NOVDSO + SKIP_BUILD=1) reproduces both worlds on demand.

## Next session, in order
1. Reproduce the 242 s paint with the vDSO on and read the [krip]/[cpu]/[msc]
   dumps DURING the pre-paint window (the periodic stall dumps only arm on unread
   input; arm them on "no present for 30 s" too).
2. The stderr-lock chain from ab2: who holds it (word=2) — dump ALL threads' last
   syscalls at that moment and find the one inside a write path.
3. Consider: vDSO mono clock at 10 ms granularity returning IDENTICAL values for
   ~10 ms stretches — chrome's delay-until-deadline math with now()==deadline
   rounding. A sub-tick component (rdtsc-scaled) in the vDSO page would give
   monotonic microsecond progress between ticks.
