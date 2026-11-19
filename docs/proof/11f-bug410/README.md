# BUG-410: the heap watch of `run_glibc_disk` took `FILES` with interrupts on, and the merge check of `feature/11f-integ2`

Branch `feature/11f-integ2`, worktree `/opt/euroos/wt-11f-integ2` on .16, not pushed. R630 run directory
`/srv/euroos/integ2-b410-runs` (CPUs 10-19 and 30-39 only, every QEMU pinned, no NIC except the Chromium runs: user-net
without hostfwd, `update.check=off`). Images built with `drivers/mkimg.sh` from `git archive` copies of each commit
(`scripts/build.sh` without the userland step; the git-ignored userland `*.elf` and `*.elf.sig` copied from the main
checkout, nothing rebuilt or re-signed, no `.sig` file of any tree made or changed; the dev key only as a symlink, removed
afterwards). One cargo target directory per source tree and kind.

| commit | what |
|---|---|
| `d18fa36` | the merge of `feature/11f-bug390` into ASLR S0-S7 (`4df79a3`), already committed before this work |
| `781537d` | tests first: `[swap-irq]` part 3; `files_heap_bytes` extracted from `run_glibc_disk` (same code, no guard) |
| `a780d84` | fix: `files_heap_bytes` holds `FILES` under `IfOffGuard` |
| (docs) | tracker (BUG-410 fixed, BUG-430 and BUG-431 filed), `docs/design/SMP-SCHEDULER.md` section 6, this directory |

## Part 1: the merge check (`d18fa36`)

**Reading.** `git show --cc d18fa36` shows no conflict hunk and no hand edit. The merge base is `55c6e25` (the S6 base
of the BUG-390 branch), so the S6 segment code is in both parents: it was already compiled and tested with the `IrqLock`
conversion on the BUG-390 branch. What the merge combined in `kernel/src/ring3.rs` is the S7 side
(`55c6e25..4df79a3`: the BUG-380 vDSO clock page code, lines 9828-10080: `vdso_tick`, `vdso_tick_one`, `VdsoWriter`,
`vdso_interp`, `vdso_rdtsc`, `vdso_clock`, the `[vdso-mono]` helpers) and the BUG-390 side (the `IrqLock` types of
`CHILD_MEM`, `DEMAND_FILE_MAPS`, `PROT_NONE_RANGES`, `SHARED_ALIASES`, `SHARED_MAPS`, `DEMAND_FREE`,
`GLIBC_FORK_CHILDREN`, `CHILD_THREADS`, `child_mem_swap`, `swap_irq_selftest`). The two sides touch disjoint regions; none
of the S7 lines names one of the eight converted locks (checked with grep over the diff `55c6e25..4df79a3`).

| check | result | file |
|---|---|---|
| cargo test (default-members), `d18fa36` | 188 suites, **1 396 passed, 0 failed**, 1 ignored (same as the BUG-390 branch) | `cargo-test-summary.txt` |
| KVM self-test boots 1/2/4 GiB, 2 vCPU, vs `4df79a3` and vs `5ec0c28` | 3 of 3 to the desktop in each image, 0 crash, **0 worse** (6 compares: WORSE 0, CHANGED 0); `[swap-irq]`, `[vdso-mono]`, `[aslr]`, `[aslr-ind]`, `[aslr-exec]`, `[aslr-phys]`, `[arena-segs]` OK in all three boots. Against `4df79a3` the new line is `[swap-irq]`; against `5ec0c28` nothing differs | `kvm-markers-compare-merge.txt` |
| `[swap-irq]` stress, 4 GiB | **30 of 30 OK** (10 each with 1, 2 and 4 vCPU), 0 crash, 0 NMI, 0 hangs, 0 other FAILED lines; 83 198 globals swaps, 171 million lookups beside them | `stress.txt` |
| gui-smoke, public image, 4 vCPU 4 GiB | **67/67** | `gui-smoke-merge.txt` |
| Chromium 5 fresh starts | **5/5** | `chrome.txt` |
| one sites round | **14/14** (YouTube "reject all" clicked, OpenStreetMap passed this time) | `chrome.txt` |

Nothing got worse through the merge; no change was needed.

## Part 2: BUG-410

### Who takes `FILES`

`FILES` (`ring3.rs`, `eurolock::sync::Mutex`, `Mode::Plain`: statistics only, no interrupt handling) has about 100 call
sites in `ring3.rs` and 4 in `fsback.rs` (no other file names it).

- **With interrupts off:** `linux_dispatch_inner_raw` and the `vfs_*` functions it calls (system calls enter with IF=0:
  FMASK), `handle_demand_fault_inner` (the page fault vector is an interrupt gate), and every function that wraps the lock
  in `IfOffGuard` or `without_interrupts`: `cdp_pump_inner`, `ram_files_census_inner`, `mem_totals`,
  `paged_named_release`, `ram_file_sweep`, `take_dirty`, `pipehold_ctl`, `fsback::fetch`, `fsback::write_back`,
  `fsback::forget_for_test`. So the hang is **possible**: ring-0 code runs on the BSP only, so a holder that the timer
  parked with interrupts on can never run again once a task with interrupts off spins on the lock (the class of BUG-007
  and BUG-390).
- **With interrupts on, while a program runs:** the heap watch of `run_glibc_disk` (task 0, the launcher loop; it sums
  names and contents of all entries once per 64 MiB step of heap use: a few microseconds to milliseconds).
- Callers that depend on who calls them: see BUG-431.

The window is small (one sum per 64 MiB step), which is why it was not seen; the test makes it as wide as it can be.

### Fix

`files_heap_bytes()` (new function, one place) takes `IfOffGuard` before `FILES` and releases the lock before the
interrupt state comes back (the sum is a statement of its own: a temporary of a tail expression would outlive the guard).
`FILES` is not converted to an `IrqLock`: its takers copy file contents under the lock and call functions that take other
locks while they hold it, and I did not review every one of them for that (the instruction said: when in doubt, the local
guard).

### Test first (`tests-first.txt`)

`[swap-irq]` part 3: a kernel task with interrupts on calls `files_heap_bytes()` without pause (the hammer task of part 2,
now with a second mode); the test task takes `FILES` with interrupts off (`files_count()` inside `without_interrupts`, as
a system call does) 2 000 times, then yields, for 3 s (300 ticks). Fails when no sum ran; hangs the machine before the fix.

| image | boots | result |
|---|---|---|
| `781537d` (test, no guard) | 5 (1 vCPU x 2, 2 vCPU x 2, 4 vCPU x 1) | **5 of 5 stop** (silent for 120 s). NMI probe: CPU 0, RFLAGS IF=0, task 0 in the pause loop of the `FILES` lock function (`eurolock::sync::Mutex<Vec<(String, Cow<[u8]>)>>::lock`, +0xa0 / +0x94) |
| `a780d84` (fix) | 30 (10 each with 1, 2, 4 vCPU) | **30 of 30 OK**; 17 998 000 takes with interrupts off beside 7 209 367 sums with interrupts on |

The part 3 test is a synthetic taker (a loop that does what a system call does), not a Chromium run; a real system call
only reaches the lock when a program runs.

### Verification of the fix (`a780d84`)

| check | result | file |
|---|---|---|
| cargo test | 188 suites, **1 396 passed, 0 failed**, 1 ignored | `cargo-test-summary.txt` |
| KVM self-test boots 1/2/4 GiB, 2 vCPU, vs the merge | 3 of 3 to the desktop, 0 crash, **0 worse** (WORSE 0, CHANGED 0, nothing only in one side); the seven markers OK; new `[swap-irq]` text with the FILES part OK | `kvm-markers-compare-fix.txt` |
| `[swap-irq]` stress, 4 GiB | **30 of 30 OK** (10 each with 1, 2 and 4 vCPU), 0 crash, 0 NMI, 0 other FAILED lines | `stress.txt` |
| gui-smoke, public image, 4 vCPU 4 GiB | **67/67** | `gui-smoke-fix.txt` |
| a run that reaches the `run_glibc_disk` path | image of the fix built with `--features chrome-boot`, chrome pack attached: `run_glibc_disk("/pack/chrome")` ran from boot, the heap watch printed `[heap] 147 MiB used \| FILES 43 MiB \| pipes 0 KiB` (the line comes from the guarded `files_heap_bytes`), the browser window mapped, 180 s later still running, 0 panic, 0 watchdog, 0 `[irqlock]` | `chrome.txt` |

The `run_glibc_disk` run shows that the path works with the guard; it does not show the old hang (the window is one
sum per 64 MiB step: the self-test is the proof of the hang).

## Other places of the same class (found by reading, none changed)

Listed in the tracker as BUG-430 (`GLIBC_THREADS`) and BUG-431 (`OUTPUT`, `THREAD_NAMES`, `GLIBC_CTIDS`, `CURRENT_APP`,
`CHILD_EXE_PATH`, `DISK_FILES`, a few more `FILES` takers). Method: the statics of `ring3.rs` that are plain mutexes, all
their `.lock()` sites, per enclosing function whether the function has an `IfOffGuard` or `without_interrupts` before the
site; sites in the system-call dispatcher and the fault handler (IF=0) taken as the "off" side. It is a scan, so it has
false positives (a function that is only reached from a system call) and false negatives (a guard that sits earlier in the
function but does not cover the site).

## Deviations

- **The first Chromium try used an older `chrome-smoke.py`** (the copy in `/srv/euroos/chrome-runs/scripts`): 2 of 5
  starts passed ("omnibox right after start ... 273 colours", the page not yet painted when counted) and a sites round
  had 9 steps. The scripts of the tree (`scripts/chrome-smoke.py`, `scripts/gui-smoke.py`) gave 5/5, 14/14 and 67/67, and
  are what the numbers above use. I did not run the old script against the base images, so I cannot say from this
  whether the 3 failures come from the script alone.
- The `[heap]` run uses an image with the `chrome-boot` feature, not the public image: the desktop browser does not go
  through `run_glibc_disk`.
- In a side try with the pack disks attached to the self-test image (to reach `run_glibc_disk` that way) the glibc
  self-tests of the image failed with exit -1 (it has no crashpad in the pack, so that path did not run either). Not
  looked into, not compared with another image; not used for any number above.
- No `.sig` file was made or changed; the images are signed for the image only (as in `11f-bug390`).

## Cleaned up

Cargo target directories (`/opt/euroos/integ2b410-target`), the work directory (`/opt/euroos/integ2b410-work`), the
`dev.key` symlinks, all images and runs on the R630 (`/srv/euroos/integ2-b410-runs`, `/dev/shm/integ2b410`). The worktree
stays.
