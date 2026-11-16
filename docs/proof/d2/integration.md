# D2, step A: integration of the 2026-11e branches on sched-d1 (2026-11-15)

Branch `feature/d2` from `feature/sched-d1` `be87a0c`, worktree `/opt/euroos/wt-d2` on .16, not pushed.
Three `--no-ff` merges, one at a time; the resolutions are in the merge messages.

| Commit | Merge | Conflicts |
|---|---|---|
| `fc0b105` | `feature/small-2026-11e` `170671a` (BUG-086/087/200/201/202/112/114/115/220, BUG-161 docs) | `docs/LOADTEST-TRACKER.md` only (sched-d1 text of BUG-192 kept, BUG-200 header of small taken) |
| `3f75d74` | `feature/kbugs-2026-11e` `9236a60` (BUG-111, BUG-089/082, BUG-088 partly) | none |
| `b6eb05d` | `feature/mem-2026-11e` `3f6bdb3` (copy-on-write fork, named RAM files in frames, sysinfo, BUG-240..243) | none textually. The expected overlap with kbugs (`glibc_arena_layout` / `aslr_slides`) is semantic only: kbugs makes every slide 4 KiB granular, the copy-on-write code keys nothing on a 2 MiB demand floor; `[aslr]` and `[memt]` (10 modes incl. `cowfork`) confirm it |

## Targeted check of `b6eb05d` (R630, run directory `/srv/euroos/d2-runs`, `integration/drivers/`)

| Test | Result |
|---|---|
| `cargo test` on .16 | 188 binaries, **1 381 passed, 0 failed**, 1 ignored (`integration/cargo-test-summary.txt`) |
| KVM self-test boots 1/2/4/8 GiB, 2 vCPU, HD-Audio | desktop in all four, 0 crash lines, 0 `[lk]` inversions. FAILED lines: `[aslr]` (expected, BUG-088 partly) in all four, the two `GBIG malloc(200 MiB)` at 1 GiB (as before) |
| markers-compare KVM, against the proof boots of every branch (validate-2026-11d, sched-d1, kbugs, small, mem) | **0 worse in 20/20** (`integration/comparison-kvm.txt`) |
| TCG matrix 10 configurations, restrict=on | **10/10 desktops, 0 watchdog, 0 NMI, 0 FROZEN, 0 fault lines, 0 inversions** |
| markers-compare TCG against validate-2026-11d | 0 worse in the six user/kernel configurations; **3 worse in the four storage/combined (1 GiB) configurations**: `[rename]`, `[rename-disk]`, `[app-policy]`: **BUG-250** (below) |
| `gui-smoke.py`, public image, KVM 4 vCPU | **PASS 67/67**, markers-compare against validate-2026-11d **0 worse** |

## BUG-250 (new): the storage configurations at 1 GiB run out of RAM-file budget

In the storage and combined TCG configurations (1 GiB, `storage.nvme=user`, RAM root) the freestanding `renprobe`
cannot write its three 1 MiB files (errno 1), `[rename-disk]` and `[app-policy]` fail after it. The log says why:
`[ramfiles] a RAM file may not grow ...: 31 MiB of kernel memory left, 32 MiB stay for the kernel (ENOSPC)`.
Reproduced under KVM (same configuration, `integration/bug250-bisect.txt`): at the first `[ramfiles]` limit line every
branch on its own has **45 MiB** of kernel memory left, `fc0b105` (sched-d1 + small) **37 MiB**, the full merge **36 MiB**.
So the 8-9 MiB come from sched-d1 and small together, not from one of them; kbugs and mem add about 1 MiB. Not
analysed further in this round (see the D2 README); the 2 GiB and larger configurations and every KVM self-test boot pass.
