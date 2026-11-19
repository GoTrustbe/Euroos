# Merge of `feature/11f-bug400` (BUG-400) into the 11f integration (`feature/11f-integ2`)

Branch `feature/11f-integ2`, worktree `/opt/euroos/wt-11f-integ2` on .16, not pushed. Merge commit `b3eda20`, first parent
`79891a2` (ASLR S0-S7, BUG-390, BUG-410), second parent `249cbf9` (`feature/11f-bug400`: the clock held to the HPET,
new `timebase.rs`, `clockdrift.rs`). The only conflict was `docs/LOADTEST-TRACKER.md` (the changelog list at the end of the
BUG-380 entry: both sides kept, one bullet each); no source file conflicted and none was edited by hand.
R630 run directory `/srv/euroos/integ2-runs/m430` (a subdirectory: `/srv/euroos/integ2-runs` held the files of an earlier
round, which I left alone). Every QEMU pinned (core + sibling), no NIC except the Chromium runs (user-net, no hostfwd,
`update.check=off`). Images built with `drivers/mkimg.sh` from `git archive` copies (`scripts/build.sh` without the
userland step; the git-ignored userland `*.elf`, `*.o` and `*.elf.sig` copied from the main checkout, nothing rebuilt or
re-signed, no `.sig` of any tree made or changed; `userland/build.sh` not run; the dev key only as a symlink in each copy,
removed afterwards). One cargo target directory per source tree and kind. The scripts used are the ones of the tree
(`scripts/chrome-smoke.py`, `gui-smoke.py`, `chrome-mem-soak.py`, `markers-compare.py`), streamed to the R630.

## What I read after the automatic merge

`git diff 79891a2 b3eda20 -- kernel/src` is 29 files of which 7 are kernel code: `timebase.rs` and `clockdrift.rs` (new),
and hunks in `ring3.rs`, `sched.rs`, `hpet.rs`, `main.rs`, `shell.rs`. The BUG-400 branch was cut from `19f7586` (the BUG-380
branch), and S7 contains that same BUG-380 code, so the hunks of BUG-400 land on the BUG-380 text that S7 carried.

| place | what it does | check |
|---|---|---|
| `ring3.rs` `vdso_tick_one` | target of the anchor is the time base (`timebase::ns_at(tsc)` plus the forced tick advances) instead of `ticks x 10 ms`; the rate is `timebase::rate_shift20()`; a lead under 1 ms is taken out over one period | sits inside the BUG-380 rules of S7 unchanged: the sequence goes odd before the anchor TSC is read, the anchor is never below what readers saw, `vdso_tick` runs it under `without_interrupts`. `[vdso-mono]` OK in every boot below |
| `sched.rs` `schedule_tick` | each timer interrupt adds the 10 ms periods the time base passed (`timebase::on_tick`, `REAL_TICKS`) | BSP interrupt context only; none of the S0-S7 or BUG-390 text is near it |
| `timebase.rs` | seqlock (`SEQ`, one writer: the BSP tick, interrupts off) over atomics, reference counter read (HPET or PM timer) | **no lock at all** (statics are `AtomicU64`/`AtomicU8`), so nothing is taken with interrupts on that something else takes with interrupts off. The one reader that can spin, `ns_at`, runs in `schedule_tick` and in `vdso_tick_one` (under `without_interrupts`), so a writer cannot be preempted under it |
| `clockdrift.rs`, `main.rs`, `shell.rs` | the `[clock-drift]` self-test is registered after `forkvdsotest::selftest` and before the `lowmem` test; `uptime` prints the clock diag line | `[clock-drift]`, `[vdso-mono]` and `[swap-irq]` are all registered and ran in every boot (table) |
| `hpet.rs` | `counter_mask` | additive |
| BUG-390 `IrqLock` conversion | not touched by the BUG-400 side (grep over the diff `19f7586..249cbf9`: none of the eight converted locks, `FILES`, `GLIBC_THREADS`, `IrqLock` or `IfOffGuard` is named; the only `without_interrupts` lines added are two closures in the `clockdrift.rs` test) | `[swap-irq]` OK |

No kernel code was changed for the merge.

## Tests (commit `b3eda20`)

| check | result | file |
|---|---|---|
| cargo test (default members) | 188 suites, **1396 passed, 0 failed**, 1 ignored (same as before the merge) | `cargo-test-summary.txt` |
| KVM self-test boots 1/2/4 GiB x 1/2/4 vCPU, 9 boots of each of the merge, `79891a2` and `249cbf9` | all 27 to the desktop, 0 crash lines, 0 NMI. markers-compare merge vs `79891a2`: **WORSE 0 in 9 of 9**, nothing changed. Merge vs `249cbf9`: WORSE 0 in 9 of 9; `[aslr-ind]` FAILED there, OK in the merge (ASLR is in the merge, not in that base), `[arena-segs]` and `[swap-irq]` only in the merge (OK). In all 9 merge boots `[clock-drift]` (3 OK lines, 0 FAILED), `[vdso-mono]`, `[swap-irq]`, `[aslr]`, `[aslr-ind]`, `[aslr-exec]`, `[aslr-phys]`, `[arena-segs]` are OK. FAILED lines left: 2 at 1 GiB, 0 above (GBIG, the known malloc 200 MiB), the same as `79891a2` | `kvm-markers-compare.txt`, `markers-kvm-2vcpu-4G.txt` |
| TCG matrix, 10 legs (2 lanes) | 10 of 10 desktops, 0 watchdog, 0 NMI, 0 crash; `[vdso-mono]` OK and `[clock-drift]` 3 OK / 0 FAILED in every leg; `[swap-irq]` OK in every leg; FAILED marker lines per leg 5/5/1/7/7 (smp1) and 5/5/1/7/7 (smp4): `[udrv-net]`, `[udrv-crash]`, `[conn]`, `[udrv-linux]` (no network behind `restrict=on`) and GBIG, the known ones | `tcg-matrix.txt`, `tcg-summary.txt` |
| drift against the host clock, 15 min, KVM | see below | `drift.txt` |
| gui-smoke, public image, KVM 4 vCPU 4 GiB | **67/67** | `gui-smoke.txt` |
| Chromium 5 fresh starts | **5/5** | `chrome.txt` |
| Chromium one sites round | **14/14** (YouTube "reject all" clicked, OpenStreetMap passed this time) | `chrome.txt` |

### Drift, 15 minutes, merge image, 883 s of measuring (goal within 50 ppm)

`vdrift.py` (`uptime` over the serial console every 20 s, the vDSO clock the processes read, against the host
CLOCK_MONOTONIC at the arrival of each line); `drift-ana.py` leaves the first sample out (the first `uptime` after the
console comes up answers 70-90 ms late, 80-100 ppm over 15 min) and takes the least-squares slope.

| 15 min | vDSO clock MONOTONIC (slope) | REALTIME (slope) | HPET in the same run | tick count | plain first-to-last |
|---|---|---|---|---|---|
| KVM at rest, 2 vCPU | **+4.7 ppm** (+0.004 s) | +4.7 ppm | +6.5 ppm | +7.4 ppm | +112.0 ppm (+0.099 s) |
| KVM, sibling core busy, 1 vCPU | **+0.1 ppm** (0.000 s) | +0.1 ppm | +1.9 ppm | +2.1 ppm | +22.8 ppm (+0.020 s) |

Both inside +-50 ppm by the slope. The plain first-to-last number of the rest run is +112 ppm: that is the late first
answer (0.09 s over 883 s); the HPET shows +124 ppm by the same method in the same run, i.e. the clock is 12 ppm from the
HPET. Host wall minus host monotonic stayed 0.000 s. Before BUG-400 the same method gave -855 (rest) and -1975 ppm (busy
siblings) over 30 min (`11f-bug400/drift.txt`). The 10 ms calibration is still off by +1261 / +2356 ppm; the time base
corrects the clock, not the LAPIC period.

## Deviations

- markers-compare does not count the `[clock-drift]` lines (they end in ASCII `-> OK`: BUG-421), so its "only in new"
  list does not show the marker; I counted it by grep (table).
- The drift runs ran beside other KVM boots on other cores (not beside the TCG matrix).
- TCG matrix legs: the FAILED-line counts are not compared against a base image; they are the known no-network ones.
- Chromium: the OpenStreetMap step (BUG-341) and "site < 3 s" (BUG-306) are known to vary; both passed here.

## Cleaned up

See `docs/proof/11f-bug430/README.md` (one clean-up for the whole session).
