# Scheduler, wave 2 part D1: S0, S1, S2 and S8 of the SMP scheduler design (2026-11-15)

Branch `feature/sched-d1` from `4dc2022` (`3ecde9e` + `docs/design/SMP-SCHEDULER.md`), worktree `/opt/euroos/wt-sched` on .16.
Not pushed. R630 run directory `/srv/euroos/sched-runs` (CPUs 0-11); serial logs and images stay there, the scripts are in
`drivers/`. Steps S3 to S7 (ring 3 on the APs, TLB shootdown, offload) are not part of this round.

| Commit | Step |
|---|---|
| `ab8e32c` | S0: measurement (`sched-stat`, `[t0-yield]`, `[if0-window]`, KVM steal time) and the tests `hogtest` (`[fair]`, `[fair-nice]`), `wakelat` (`[wakelat]`, new glibc program `gwakelat`), `if0test`, `hogs` |
| `cdf5aff` | S1: time-based fairness on one CPU (vruntime on the TSC, nice weights, placement floor, desktop at nice -10) |
| `2ef9d7b` | S2, the test first: `[wakelat]` with a waker that runs on after the wake (the S0 version could not tell waker and woken apart) |
| `80c0d69` | S2: `IPI_RESCHED` (0x42) and wake preemption (1 ms); `WAKE_YIELD`/`wakeyield` removed |
| `8fe1de9` | S8: a disk wait with IF=0 waits for the MSI-X completion with interrupts on (BUG-182, H1) |
| (last) | this proof and the tracker (BUG-146 and 182 updated, BUG-192 fixed, BUG-230 and BUG-231 new) |

Tests ran in KVM guests on the R630 (`drivers/sched.py`: boot, serial console commands, `-nic none`), public image unless said.

## 1. S0: numbers

| Measurement | Result |
|---|---|
| BUG-192 mechanism, `[fair]` hogtest 5 + churn proc 8 | self-test image, 16 GiB, 30 rounds: churn **7.1 s** (alone 3.9 s), the desktop task **9 %** of the run time; public image: task 0 and its glibc processes **28 %** -> FAILED (want >= 50 %) |
| who the "five tasks" of BUG-192 are | `sched-stat` on the self-test image: ten equal ring-3 spinners while `hogtest 5` runs: the five hogs and the **five counter demo tasks of the self-test image** (nice 0, never block). These are the t7-t11 of the soak analysis |
| BUG-192 in < 3 min | the starvation is reproduced in seconds by `[fair]` (9 % / 28 %). The 32-44 s rounds themselves need the soak (27-minute period, a 2 h soak below) |
| BUG-182 on demand | throttled virtio disk (`throttling.iops-total=4`): `if0test` **241 ms** without a tick, `[if0-window] 239 ms cause=blk`. Host steal 0 ms in every window seen: H1, not H2 |
| other windows (`if0-window.txt`) | self-test boot: eleven of 81-131 ms `cause=syscall 28` (madvise, IF=0) and three of 110-218 ms in kernel code: **BUG-230** (new) |
| S7 gate: kernel share of the browser during keylat | **26 % and 28 %** (S0, two sessions; S1 23 %, S2 27 %): below 40 %, S7 is not pulled before S5. Measured on the TSC per system call and demand fault: a system call runs with IF=0 and a tick never samples it (the tick ledger says 0 %) |
| `[fair-nice]` | 1.24:1 (want 3.06:1) FAILED |
| `[wakelat]` (2ef9d7b version, on S1) | wake to run p50 3.07 ms, p99 3.12 ms FAILED |

## 2. Criteria per step, before -> after

| Step | Criterion | Before | After |
|---|---|---|---|
| S1 | `[fair]` share >= 50 % | public 28 %; self-test 9 % | public **median 53 %** (10 rounds, smp 1 and smp 4; lowest 47 %, the first round: parked hogs start half a slice ahead); self-test **43-47 %** (ten busy nice-0 tasks there: entitled 48 %) -> met on the public image, not by the lowest round, not on the self-test image (explained, not tuned away) |
| S1 | churn worst <= 12.6 s, 30 rounds with hogs, smp 1 and smp 4 | self-test 7.2 s worst (median 7.1 s) | **1.6 s worst** (median 1.5 s) on smp 4 and smp 1; `churn proc 8` alone 3.9 -> 1.1 s |
| S1 | `[fair-nice]` 3:1 +-15 % | 1.24:1 | **3.03:1** |
| S1/S2 | matrix 0 worse | | TCG 10/10, markers-compare **0 worse** (section 3) |
| S2 | `[wakelat]` p50 < 1 ms, p99 < 12 ms | 3.07 / 3.12 ms | **0.05 / 0.10 ms**, hogs keep 87 % of their CPU share (>= 85 %) |
| S2 | keylat woken tasks >= 80 % under 1 ms | 24 %, 43 % | **81 %** (518 of 816 under 0.1 ms) |
| S2 | keylat harness median (information; design: not worse than off) | S0 360, 367, 334 ms (kernel total 196, 192, 219 ms) | S1 385; S2 395, 413; final 370 ms beside S0 334 ms in the same hour (kernel total 212, 224 ms): **about 10 % slower, not met**; the kernel path did not change, wake waits did |
| S8 | `[if0-window]` throttled disk, no window > 50 ms | 241 ms | **10 ms** (twice) |

`step-tests.txt` has every line; `keylat.txt` the browser sessions.

## 3. Regression of `8fe1de9`

| Test | Result |
|---|---|
| `cargo test` on .16 | 187 binaries, **1 363 passed, 0 failed**, 1 ignored (identical to validate-2026-11d) |
| KVM self-test boots 1, 2, 4, 8 GiB, HD-Audio (`kvm-selftest-boots.txt`) | desktop in all four, 0 crash lines, 0 `[lk]` inversions; FAILED lines 2/0/0/0 (the two `GBIG malloc(200 MiB)` at 1 GiB, as before); markers-compare against validate-2026-11d **0 worse** at all four |
| TCG matrix of 10, restrict=on (`tcg-summary.txt`) | **10/10 desktops, 0 watchdog, 0 NMI, 0 FROZEN, 0 fault lines, 0 inversions**; markers-compare against validate-2026-11d **0 worse in 10/10** |
| q35 matrix, KVM, restrict=on (`q35-matrix.txt`) | the 13 legs of the validator **13/13** (12 in the full run; `printer` again alone: the full run of the script stopped at the `tpm` leg, `swtpm` is not installed on the R630). `nvmeroot`/`ahciroot` (not in the 13) fail the same way on the image of `a38230d` |
| `gui-smoke.py`, public image | **PASS 67/67**; markers-compare against validate-2026-11d **0 worse** |
| Chromium site matrix, 3 rounds (`chrome-sites3.txt`) | **PASS 36/36** |
| interaction matrix (`chrome-interact.txt`) | **PASS 18/18** |
| local video (`chrome-localvideo.txt`) | VP9+Opus: picture 28/28, currentTime 1.8 -> 60.3 s, **sound 12 of 12 windows**; Opus alone 12/12; VP9 alone OK; H.264+AAC fails its 5 steps by design (BUG-161), 16/21 as before |
| loadtest soak 2 h, 16 GiB, 4 vCPU (`r630-soak-summary.txt`, `r630-soak-compare.txt`) | **11 729 commands ok, 0 errors** (validate-2026-11d: 3 513), 0 panic/fault/NMI/watchdog/FROZEN, 0 `[lk]` inversions, `[churn-proc]` 8 648 verdicts 0 FAILED, heap +0.32 kB/min, tasks/files/pools flat. **`churn proc` median 1.3 s, worst 3.3 s** (validate-2026-11d: 6.6 s and 43.9 s; limit 12.6 s): BUG-192 met. One desktop gap over 10 s (10.2 s, the first `churn proc`). **Worse: the inbound HTTP hammer**, p50 5.4 -> 60.9 ms, 827 487 -> 440 262 requests, 0 errors: BUG-231 (new, open). The commands now run 3.3 times as fast, so the CPU is busier |

## 4. Deviations and what was not done

- S8 does not block: a caller with IF=0 keeps the CPU while it waits with interrupts on (switching away inside an IF=0 system
  call would break its atomicity and the swapped globals); a caller with IF=1 is preemptible as before.
- `[fair]` counts the share of all run time, other kernel tasks included (the serial TX task at nice -10 takes 15-20 % while the
  churn prints). Against the hogs alone the desktop gets about two thirds. The lowest-round and self-test numbers miss 50 %.
- `[wakelat]` was changed after S0 (`2ef9d7b`); the hogs' share is measured on the TSC, the counter rate followed host load.
- The S1 commit message names 28 % as the S0 share: that is the first S0 classification (glibc processes counted as "other").
- BUG-231 is not analysed: the hammer clients are served by kernel tasks that now compete with a desktop task at nice -10 that
  runs three times as many console commands per hour; whether the httpd path lost CPU or waits elsewhere is open.
- The TCG matrix ran once, at the end, for S1, S2 and S8 together; per step only the targeted tests ran.
- q35 legs `tpm`/`tpmcrb` could not run (no `swtpm`); the 13 legs of earlier proofs passed.
- `chrome-smoke.py` in the run directory has two `scon` calls more (sched-stat and keylat around the 30 keys,
  `drivers/chrome-smoke-sched-stat.diff`); `scripts/` in the repository is unchanged.
- `gwakelat.sig` is the signature of the new test binary with the daily key; no other signature changed. `dev.key` was a symlink
  for the builds and is removed. `userland/*.elf` that git ignores were copied from the main checkout.
- Images were streamed .16 -> laptop -> R630 without being stored on the laptop.
