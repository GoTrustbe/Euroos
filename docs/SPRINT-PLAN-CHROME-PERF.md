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
