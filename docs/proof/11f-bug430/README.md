# BUG-430: GLIBC_THREADS taken with interrupts on by the launchers, kill_glibc_group and the CDP frame (11f-bug430)

Branch `feature/11f-integ2`, worktree `/opt/euroos/wt-11f-integ2` on .16, not pushed, on top of the merge `b3eda20`
(`docs/proof/11f-integ2/`). R630 run directory `/srv/euroos/integ2-runs/m430` (CPUs pinned, no NIC except the Chromium runs:
user-net without hostfwd, `update.check=off`). Images built as in `11f-integ2` (`drivers/mkimg.sh`, no `.sig` made or
changed, dev key only as a symlink, removed afterwards).

| commit | what |
|---|---|
| `acd65ba` | tests first: `[swap-irq]` part 4; `glibc_threads_snapshot/_len/_clear/_forget` extracted, still without a guard (the takers with interrupts on call them; they work on a copy instead of iterating under the lock) |
| `9000469` | fix: `IfOffGuard` inside the four functions; the one guarded site of `cdp_pump_inner` moved to `glibc_threads_snapshot` |
| (docs) | tracker (BUG-430 fixed, BUG-440 new, BUG-431 extended), `docs/design/SMP-SCHEDULER.md` section 6, this directory |

## Which places really run with interrupts on (checked per site)

`GLIBC_THREADS` is a plain `spin::Mutex` in `ring3.rs` (no other file names it). Ring-0 code runs on the BSP only, so a
holder that the timer parked with interrupts on cannot run again while a task with interrupts off spins on the lock.

| place | interrupts | evidence |
|---|---|---|
| `cdp_begin_frame` | **on** | called from `cdp_pump_inner` <- `cdp_pump` <- the desktop loop (`main.rs`) and `run_glibc_disk`: task 0; the neighbouring code of `cdp_pump_inner` says "this runs on task 0 with them on". It iterated the list while printing (`serial_println!`, `thread_name`) |
| `run_glibc_disk` (stall snapshot `len`, the two end-of-run loops) | **on** | task 0 launcher loop (ends in `hlt`; BUG-410). The loops called `free_thread_kstack` and `reclaim_task` with the lock held |
| `kill_glibc_group` | **on** | called by the two launchers on a timeout, task 0 |
| `spawn_glibc_persistent` (`clear`) | **on** | BUG-040: the launch code runs with interrupts on (a tick between spawn and the last store ran the program) |
| `run_glibc` (`clear`, timeout dump, two end-of-run loops) | **on** | same, and the `[swap-irq]` test itself calls it with interrupts on |
| not in the list, found: `glibc_disk_launch` (`clear`), `kill_persistent_glibc` (loop + `clear`; the browser is closed), `retire_task` (`retain`) | **on** | `retire_task` is reached with interrupts off from `kill(2)` and `exit` and with interrupts on from `reap_orphan_fork_children` / `fork_child_teardown` (end of a run, `kill_persistent_glibc`) |
| `linux_dispatch_inner_raw` (clone, exit, exit_group, worker exit), `proc_tasks`, `set_process_profile`, `fault_kill_group` | off | system calls enter with IF=0 (FMASK); `proc_tasks` and `set_process_profile` are called from the dispatcher (`seccomp_install_filter`, `prctl`, the clone limit), `fault_kill_group` from the fault vectors (interrupt gates) |
| `futex_purge_task` | n/a | **false positive**: it takes `FUTEX_QUEUE`, not `GLIBC_THREADS` |
| `dump_threads_now` | n/a | does not take `GLIBC_THREADS` at all (the tracker text was wrong there) |
| the guarded site of `cdp_pump_inner` | broken | `{ let _g = IfOffGuard::new(); GLIBC_THREADS.lock().clone() }` drops `_g` before the lock temporary of the tail expression (edition 2021; checked with a small program): the lock was released with interrupts on. Fixed by using the same function (see BUG-440 for the three like sites that I did not change) |

## Fix (`9000469`)

Four functions next to `files_heap_bytes`: `glibc_threads_snapshot` (copy), `glibc_threads_len`, `glibc_threads_clear`,
`glibc_threads_forget(t)`. Each takes `IfOffGuard` first, takes the lock in a statement of its own (so it is released
before the guard restores the interrupt state), and does nothing else under it. Every taker with interrupts on goes
through them: the loops that iterate and then call `mark_dead`, `reclaim_task`, `free_thread_kstack`, `last_syscall`,
`serial_println!` now iterate a copy, without the lock. The takers with interrupts off are unchanged.

`GLIBC_THREADS` is **not** made an `IrqLock`: the dispatcher sites (exit_group, the worker exit) hold it while they print and
free kernel stacks, and I did not want to change code that runs with interrupts off on every thread exit.

The copy is a snapshot: a worker that is cloned or exits while a launcher walks its copy is not in the copy or is in it
once more; every use of the list is idempotent (`mark_dead`, `reclaim_task`, `free_thread_kstack` are, as their comments say;
the `kill_*` functions skip tasks that are already dead).

## Test first (`tests-first.txt`)

`[swap-irq]` part 4: a kernel task with interrupts on calls `glibc_threads_snapshot().len() + glibc_threads_len()` and
`glibc_threads_forget(usize::MAX)` without pause (the list has 32 dummy entries, so the critical section is not empty; no
program runs, the list is emptied again afterwards); the test task takes the lock with interrupts off (`len`, as a system
call does) 2 000 times, then yields, for 3 s. Fails when no copy ran; hangs the machine before the fix.

| image | boots | result |
|---|---|---|
| `acd65ba` (test, no guard) | 5 (1 vCPU x 2, 2 vCPU x 2, 4 vCPU x 1) | **5 of 5 stop** (silent 120 s). NMI probe: CPU 0, RFLAGS IF=0, RIP in the spin loop inside `swap_irq_selftest` (`cli`, `lock cmpxchg` on the byte `0x14416ef28`, `pause`), and that address is `GLIBC_THREADS` in the link map |
| `9000469` (fix) | 30 (10 each with 1, 2, 4 vCPU) | **30 of 30 OK**; 17 968 000 takes with interrupts off beside 88 241 754 launcher-style copies with interrupts on |

The test is synthetic (a loop doing what the launchers do), not a Chromium run. A real program reaches these sites at
a launch, at the end of a run and in `cdp_begin_frame` (one per capture request).

## Verification of the fix

| check | result | file |
|---|---|---|
| cargo test | 188 suites, **1396 passed, 0 failed**, 1 ignored | `cargo-test-summary.txt` |
| KVM self-test boots 1/2/4 GiB x 1/2/4 vCPU, vs the merge `b3eda20` | 9 of 9 to the desktop, 0 crash, **0 worse** (WORSE 0, CHANGED 0, nothing only in one side); `[clock-drift]`, `[vdso-mono]`, `[swap-irq]` (with the new part), `[aslr]`, `[aslr-ind]`, `[aslr-exec]`, `[aslr-phys]`, `[arena-segs]` OK in all 9 | `kvm-markers-compare-fix.txt` |
| `[swap-irq]` stress, 4 GiB | **30 of 30 OK** (10 each with 1, 2 and 4 vCPU), 0 crash, 0 NMI, 0 `[irqlock]`, 0 other FAILED lines | `stress.txt` |
| gui-smoke, public image, 4 vCPU 4 GiB | **67/67** | `gui-smoke-fix.txt` |
| Chromium 10 fresh starts | **10/10** | `chrome.txt` |
| Chromium one sites round | **14/14** | `chrome.txt` |
| chrome-mem-soak 1 h, KVM 2 vCPU 8 GiB, local video clips VP9 + Opus only | **PASS**: 5 rounds in 3928 s (the 14-site matrix, the local clips, a census per round), 123 of 123 steps ok, 0 FAIL; serial log: 0 ring-0 faults or panics, 0 watchdog, 0 `[timer-probe] FROZEN`, 0 screen frozen, 0 `[irqlock]`, 0 allocation failures; picture advances in 10 of 10 checks (28 of 28 screendumps), sound in 10 of 10; clip map without `h264.mp4`; heap 123 MiB used and files 43 MiB in every round (no growth) | `chrome.txt`, `soak.txt` |

## Other places of the same class (found by reading, none changed)

- **BUG-431** (open, extended in the tracker): in the desktop browser path `cdp_begin_frame` still calls `thread_name`
  (`THREAD_NAMES`, no guard; the system call that names a thread takes it with interrupts off); `retire_task` takes
  `GLIBC_CTIDS` and `THREAD_NAMES` without a guard and is reached with interrupts on from `kill_persistent_glibc` ->
  `reap_orphan_fork_children` -> `fork_child_teardown`; `fork_child_teardown` takes `GLIBC_CHILD_EXITS` the same way.
- **BUG-440** (new, open): the same defect that I fixed at the one `GLIBC_THREADS` site of `cdp_pump_inner`: the guard is
  released before the lock at `cdp_pump_inner` (`FILES` at the `files_mib` sum, `THREAD_NAMES` before the `[rmain]` line,
  `SOCK_PAIRS` in the fd-peer lookup) and `dump_threads_now` (`THREAD_NAMES`). The window is a few instructions.

## Deviations

- The test commit also moved the unguarded takers to copies (a loop over a copy instead of over the locked list); that is
  a small behaviour change that is not the guard, and it is in the test commit because the test calls those functions.
- The `GLIBC_THREADS` takers with interrupts off were read, not changed; the lock is not an `IrqLock` (see above).
- No `[irqlock]` line can occur here (`GLIBC_THREADS` is not an `IrqLock`); the stress and the soak count the word anyway.
- The fix commit message was amended once (message only, same tree) right after it was made, before any later commit.
- **`cdp_begin_frame` itself did not run in any browser run** (0 "[cdp] capture requested" lines): the desktop loop does not
  drive the DevTools conversation in these runs. The soak and the Chromium runs exercise the launch (`spawn_glibc_persistent`,
  `glibc_threads_clear`), the fork-child reap (`retire_task` -> `glibc_threads_forget`) and clone/exit with the lock
  taken with interrupts off; the sites in `cdp_begin_frame`, `run_glibc_disk` and `kill_glibc_group` are covered by the
  synthetic test only (and `run_glibc` by the boot self-tests, 9 + 30 boots).
- The Chromium 5-start run on the merge and the 10-start run on the fix used the scripts of the tree, as asked.
- BUG-421 again: markers-compare does not count `[clock-drift]`, so that marker was counted by grep.

## Cleaned up

Cargo target directories (`/opt/euroos/integ2m-target`), the work directory (`/opt/euroos/integ2m-work`, with the
`git archive` copies and their `dev.key` symlinks), all images and runs on the R630 (`/srv/euroos/integ2-runs/m430`,
`/dev/shm/m430`), temporary files in `/tmp` of .16 and of the laptop. The worktree stays. `/srv/euroos/integ2-runs` itself
held earlier files of another round before I began (a run directory with that name already existed); they were left alone.
