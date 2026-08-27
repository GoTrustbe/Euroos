# Sprint 4: make the machine cheap enough for the browser

## Standing
Input delivery is proven correct end to end (sprint 3). The browser is not stuck and
not starved: its main thread gets the largest user-mode CPU share (33%), yet ALL user
code together gets ~9.5 s out of every 45 s of wall time. The rest is kernel work.
Until startup ends, the main thread does not return to its X event loop, and the
click waits unread in the socket. The lever is the cost per page fault and per
PutImage — kernel work, our work.

## Where the cycles measurably go
- **Demand paging, 4 KiB per device round-trip.** `handle_demand_fault` fills ONE
  page per fault; each disk fill is a virtio request capped at `DATA_MAX = 4096`
  with a `kick_and_wait` busy-wait. A 495 MB binary demand-paged this way is
  ~120 000 device round-trips, each a VM exit under TCG.
- **PutImage copied twice.** A 259 KB request is `extend_from_slice`d into `inbuf`,
  then `drain(0..len).collect()`ed into a fresh Vec before processing — half a
  megabyte of memmove per painted frame, before the blit even starts.

## Rule for this sprint
Measure before and after every change, in the same boot, with counters the kernel
prints itself. A change without a number gets reverted.

## Phase A — instrument (the before-numbers)
- A1. Fault counters in the heartbeat: faults total, disk-filled, cumulative µs in
  `handle_demand_fault` (rdtsc), µs in `kick_and_wait`.
- A2. X counters: PutImage count + bytes, `process()` µs cumulative.
- Milestone: one `[hb]` line says where kernel time goes. No behavior change.

## Phase B — 64 KiB per device round-trip
- B1. The virtio-blk data buffer becomes 16 contiguous frames (64 KiB);
  `DATA_MAX = 65536`. The queue layout is untouched (one data descriptor, bigger).
- B2. `disk_read_bytes` reads in up-to-64 KiB chunks instead of 4 KiB.
- Milestone: the same boot issues ~16x fewer virtio kicks (counter proves it).

## Phase C — fault read-ahead (cluster fill)
- C1. On a DISK-backed demand fault, fill and map a whole aligned 64 KiB cluster
  (16 pages) inside the same mapping, in one disk read: code and data runs are
  sequential, so the next faults were coming anyway.
- C2. Anonymous and RAM-file faults stay single-page (no evidence they matter).
- Milestone: faults-per-second drops ~10x during chrome startup (counter).

## Phase D — one copy fewer in the X pipeline (only if A says it matters)
- D1. `process()` parses requests in place from `inbuf` and drains ONCE at the end,
  instead of drain-collecting every request into a fresh Vec.
- Milestone: PutImage bytes-copied halves (counter).

## Phase E — the payoff, measured end to end
- E1. chrome-iter boot: wall time from launch to first full-window present, before
  vs after (the runner logs presents with timestamps already).
- E2. The click: does the tab-strip click get COLLECTED (client reads after the
  event, queue drains) within the run.
- E3. Host tests still green; a non-chrome boot still reaches the desktop.

## Results (2026-08-22, all measured)

| change | number |
|---|---|
| Phase A ledger (baseline) | 88% of fault time = virtio busy-wait (15 071 of 17 067 Mcyc, 33 705 kicks); X processing 1.5 Gcyc = noise |
| Phase B (64 KiB reads) | kicks 3.2x down — wall-clock NEUTRAL: the wait is proportional to bytes under TCG |
| Phase C (read-ahead) | 54% over-fetch; wall-clock neutral. Kept: right on real hardware, refuted as the bottleneck here |
| Ring-0 profiler | the REAL bottleneck: 83% of ring-0 ticks in the yield/context-switch path |
| Idle-hlt + 1-in-4 recvmsg yield | **first full paint 11 s → 6 s after map** (from 4 min at sprint start); yield path 83% → 66% (remainder includes the idle hlt itself, which shares the page) |
| Phase E3 | 992 host tests pass, 0 failed |

## The verdict that ends this sprint

The long-patience run (4 clicks over 18 minutes, screenshots between) settles it:
ZERO events collected, ZERO X requests after the first paint burst, ~20 syscalls/s,
16–30% user CPU still burning, guest clock racing 4.3x during startup (the forced
tick-advance in the epoll retry loop — a real smell, noted for later).

Chrome's main thread is not starting up slowly. It is in a USER-SPACE loop that
never completes and almost never syscalls — the stall profile puts 21% of its
samples on one libc page, which at page granularity is consistent with glibc's
adaptive mutex spin. The leading suspect is a lost futex wake (the same class as
the thread-pool deadlock this project has beaten before): the holder of a lock is
parked and the spinner never gets its wake.

Sprint 4's job — make the machine cheap — is done and measured. The next sprint is
correctness again: trace futex wait/wake pairs on the main thread, find the wake
that never arrives.
