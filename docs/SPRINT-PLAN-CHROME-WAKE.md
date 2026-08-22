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
