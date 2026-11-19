# BUG-390: the browser main thread spins with interrupts off in `child_mem_swap`

Branch `feature/11f-bug390`, worktree `/opt/euroos/wt-11f-b390` on .16, from `feature/11f-aslr-s6` `55c6e25`, not pushed.
R630 run directory `/srv/euroos/b390-runs` (CPUs 8-17 and 28-37 only). Images built with `drivers/mkimg.sh`
(`scripts/build.sh` without the userland step; userland binaries and signatures copied from the main checkout, none
rebuilt or re-signed; one cargo target directory per source tree and kind). fix = the tree of `9692f11`, base = `55c6e25`.

| commit | what |
|---|---|
| `378f83d` | tests first: self-test `[swap-irq]`, shell command `swapirq [rounds]`, counter `CHILD_MEM_SWAPS` |
| `9692f11` | fix: `irqlock::IrqLock`; the eight locks of the swapped demand state are IrqLocks |
| (docs) | tracker BUG-390 (fixed), BUG-410 (new, open), `docs/design/SMP-SCHEDULER.md` section 6, this directory |

## Cause (proven)

1. **Where it spun.** `7b2eea6` (the image of the hang) rebuilt bit for bit: its link map has the same addresses as the
   map the probe was symbolised with. `child_mem_swap+0x1ab` is the `pause` loop of the spin lock on
   **`DEMAND_FILE_MAPS`**, the first of the five lists after `CHILD_MEM` (`analysis.txt`, with the disassembly).
2. **Why it could not end.** `child_mem_swap` runs with interrupts off, and ring-0 code runs on the BSP only (system calls
   and faults of threads on an AP are sent back to the BSP). A holder of the lock that the timer preempted with interrupts
   on can never run again once a task with interrupts off spins on that lock. The `fork_child_owner` locks inside
   `child_mem_swap` were free a moment earlier in the same interrupts-off window (`ensure_globals_inner`), so the lock
   held was `CHILD_MEM` or one of the lists, and the disassembly says which.
3. **Who held it.** The desktop loop (task 0, interrupts on) takes `DEMAND_FILE_MAPS` in `demand_addr_origin`, up to 16
   times per `dump_rip_profile`, which `dump_threads_now` calls from the CDP heartbeat census (heartbeats 12 and 20),
   "silent after navigate", "cdp channel dead", and the stall profile of `run_glibc_disk`. The `[prof]` line of the same
   heartbeat already wraps that call in `IfOffGuard` (runs 21 and 53); `dump_rip_profile` did not. Soaks print 2 to 5
   such dumps an hour (`rip-dumps` in `chrome.txt`), each holding the lock for microseconds: rare, as seen.

Ruled out by reading: a disk wait that yields while holding a list lock (virtio waits spin, no yield); kernel work on
an AP taking them (the only `kwork` job is the serial drain); a deadlock between two CPUs (no second CPU runs ring-0
code); the page-fault handler as holder with interrupts on (an interrupt gate: IF=0 throughout). Not found when reading
the lock sites (not proven absent for every path): a yield inside a critical section of these locks.

## Tests first (`tests-first.txt`)

`[swap-irq]`: (1) each of the eight locks taken from a context with interrupts on must be held with interrupts off;
(2) `gslots` (40 fork children with 6 threads each, ended by exit_group, a worker's exit_group and kill, the parent's
threads beside them) runs while a kernel task with interrupts on calls `demand_addr_origin` without pause.

| image | boots | result |
|---|---|---|
| `378f83d` (tests, no fix) | 5 (2 vCPU x 2, 1 vCPU x 2, 4 vCPU x 1) | **5 of 5 stop** in (2). NMI: IF=0 spin on `DEMAND_FILE_MAPS` in `child_mem_swap+0x243` (4 vCPU: task 0 swapping the globals of fork child 12 back) or `ro_file_note+0x45` (mprotect); the holder the kernel task, Ready with IF=1 inside `demand_addr_origin` |
| fix | 2 (2 vCPU) | OK: none held with interrupts on; gslots exit 88, 2 366 and 2 360 globals swaps, 5.9 and 6.1 million lookups beside them |

## Results (fix)

| check | result | file |
|---|---|---|
| cargo test (default-members) | 188 suites, **1 396 passed, 0 failed**, 1 ignored (same as `55c6e25`; the new test is a kernel self-test) | `cargo-test-summary.txt` |
| `[swap-irq]` stress, KVM, 4 GiB | **90 of 90 OK** (30 each with 1, 2 and 4 vCPU), 0 crash, 0 NMI, 0 hangs; 238 196 globals swaps, 508 million lookups beside them; other FAILED lines: `[vdso-mono]` 7 times (BUG-380) | `stress.txt` |
| KVM self-test 1/2/4/8 GiB, 2 vCPU, HD-Audio, vs `55c6e25` | 4/4 to the desktop, 0 crash, **0 worse**; new `[swap-irq]` OK | `kvm-markers-compare.txt` |
| gui-smoke, public image, 4 vCPU 4 GiB | **67/67** | `chrome.txt` |
| Chromium 20 starts, 2 vCPU | **20/20**, 188 execs, 0 aborts, 0 ring-0, 0 FROZEN, 0 `[oom]` | `chrome.txt` |
| sites, 3 rounds in one session | **36/36** (YouTube "reject all" clicked) | `chrome.txt` |
| chrome-mem-soak 1 h, 8 GiB, six runs (two waves of three) | 6 of 6 ran 5 rounds to the end: **0 hangs, 0 ring-0 faults, 0 watchdogs**, 0 FROZEN, 0 aborts, 0 `[oom]`, 0 `[irqlock]` lines; 2 PASS (123/123), 4 FAIL by single steps: YouTube 3x, OpenStreetMap 4x (see deviations); local VP9+Opus picture and sound in every round of every soak | `chrome.txt` |

The path of the hang ran in every soak: 2 to 5 `dump_rip_profile` calls from the heartbeat each (`rip-dumps`).

## 2026.11.17 (`b30adb6`)

Same code, so the same risk: `DEMAND_FILE_MAPS` is a plain spin lock, `demand_addr_origin` takes it, and
`dump_rip_profile` calls it without `IfOffGuard` from the heartbeat census (`census at heartbeat`). Checked by reading
(`git show b30adb6:kernel/src/ring3.rs`), not run.

## Deviations and open points

- **No Chromium reproduction.** The hang itself was seen once (the S6 base soak); it was reproduced with the self-test,
  which takes the lock the way the heartbeat does, not in a browser run. Six soaks with the fix did not hang; that
  alone would not prove much, the self-test does.
- **YouTube, first visit, wave 1 (3 of 3 soaks) and the base round (`sites-s6`).** The page is right (screenshots
  `youtube-soak1-a-fix.png`, `youtube-sites-s6-base.png`: "Your YouTube history is off" after "reject all"); the step
  failed on the desktop-loop stall rule (slowest console answer 1 130 to 1 545 ms, limit 1 000 ms) while three soaks,
  the stress boots and other guests shared the host. The base image failed the same step the same way (1 246 ms) beside
  them; with fewer guests (wave 2, `sites3`) the same visit took 433 to 697 ms and passed. Not caused by the fix.
- **OpenStreetMap** failed 4 times (2 soaks; "still changing"): BUG-341.
- **`[vdso-mono]`** FAILED in 7 of 90 stress boots: BUG-380 (the clock; another team), not touched here.
- **Soak round time** 3 946 to 4 112 s for 5 rounds (S6: 3 922 s) with three soaks side by side and the other runs.
- The tests-first `t1-smp1-2` boot shared CPU 35 with `t1-smp4-1`.
- Interaction (`--only interact`), the TCG matrix, q35 and installs were not run (not asked for this fix).
- The kernel images are signed for the image by `drivers/mkimg.sh`; no `.sig` file of the tree was made or changed.

## New bugs

BUG-410: `FILES` taken with interrupts on in the heap watch of `run_glibc_disk` (same class, not the desktop browser;
open, not changed). BUG-411..419 not used.
