# Integration of sprint 2026-11f (11f-keylat, 11f-mem, 11f-small)

Branch `feature/11f-integ` from `e4b8ba4`, worktree `/opt/euroos/wt-11f-integ` on .16. Not pushed. R630 run directory
`/srv/euroos/11fi-runs`, work directories of the timing runs on tmpfs (`/dev/shm/11fi`).

## Merges (real merge commits, order A, B, C)

| commit | merge | conflicts |
|---|---|---|
| `54d9e79` | `feature/11f-keylat` (`08f9b5a`: code up to `4e681ba` + its proof) | none |
| `cae3f5b` | `feature/11f-mem` (`88438e1`) | `docs/LOADTEST-TRACKER.md`: both appended entries; kept BUG-300..307 and BUG-310 |
| `42530b3` | `feature/11f-small` (`c68192b`) | `docs/LOADTEST-TRACKER.md`: BUG-282 takes 11f-small's FIXED entry (its old `Next:` line dropped), BUG-300..307/310 kept after it |

`serial.rs`, `xserver.rs`, `main.rs`, `ring3.rs`, `chrome-smoke.py` merged without conflicts; the code around them was read
(below).

## Interactions checked

1. **Serial TX on an AP (A) + bounded log lines (B) + emergency drain (BUG-270).** `serial::emergency()` sets EMERGENCY,
   after which `ap_drain_submit` refuses, and empties the whole queue under the UART lock; power-off and the panic paths call
   it. An AP drain that already took a load (<= 16 bytes) can still write that load to the port after the flush started
   (`finish_queue` does not wait for `TX_PORT`): at most one load out of order, nothing lost. Filed as **BUG-330** (LOW, open).
   q35 `power` leg PASS; markers-compare finds no line missing at 1/2/4/8 GiB. B's 160-char cap is on the Euro `log()`
   path only and does not touch the queue.
2. **Direct wakes (A) + XCONNS with interrupts off (C).** The other X server lock users in task 0 were checked:
   `pump_keyboard`, `deliver_*`, `queued_fds` (A's new waker), `keymap_changed`, `hosted_resize`, `with_front_window`,
   `front_window_size` already hold them with IF=0; `dump_input_state` runs under `IfOffGuard`. One was missed:
   **`forget_clients`** (desktop loop, when a hosted app starts) took XCONNS, PRESENTED and the selection tables with
   interrupts on. **BUG-331**: test first, `[xlock]` now also runs `forget_clients` (when no connection is open):
   `= 1 (want 0) -> FAILED` on `eb239c5` (KVM 2 GiB, `xlock.txt`), fix `7253cb1` (`without_interrupts`): `= 0 -> OK` in all
   KVM boots and all 100 TCG boots.
3. **Desktop-loop stall on YouTube (B, before its lock-order fix).** Sites run of the merge, 3 rounds: slowest console
   answer of the desktop loop 449 ms (github r3), YouTube 336-432 ms, median 12 ms. No stall near 1.5 s (`chrome-sites.txt`).

## Results (final code `7253cb1`)

| check | result | file |
|---|---|---|
| cargo test (default-members) | 188 suites, 1381 passed, 0 failed | `cargo-test-summary.txt` |
| KVM self-test boots 1/2/4/8 GiB, 2 vCPU, HD-Audio | desktop 4/4, 0 crash lines, FAILED lines 3/1/1/1 as on `e4b8ba4`; markers-compare **0 worse** at all four sizes, new: `[xlock]` OK | `kvm-markers-compare.txt` |
| q35 matrix, KVM, restrict=on, 15 legs incl. `power`, `nvmeroot`, `ahciroot` | **15/15 PASS** | `q35-matrix.txt` |
| gui-smoke, public image, 4 vCPU 4 GiB | **67/67** | `gui-smoke.txt` |
| 100 TCG boots combined-smp1-iommu-1g, sibling load | **100/100 desktop**, 0 watchdog, 0 NMI, 0 crash, 0 `[fuzz]`/`[churn]` FAILED, `[churn-forkthreads]` 40/40 in 100/100 | `tcg100.txt` |
| boot time, public, 2 vCPU 2 GiB, pin 18-19, quiet host, 8 alternating runs each | median **2.60 s** (2.19-2.67) vs `e4b8ba4` **2.55 s** (2.18-2.95) | `boot-time.txt` |
| Chromium 20 starts, smp 2 | **20/20, 0 FROZEN** | `chrome-starts20.txt` |
| sites, 3 rounds, smp 2 | **36/36** | `chrome-sites.txt` |
| interaction, smp 2 | **18/18** | `chrome-interact.txt` |
| keylat smp 4, fbread, 3 pairs | merge 92 91 98 ms vs `e4b8ba4` 93 80 95 ms | `keylat.txt` |
| keylat smp 2, fbread, 3 pairs alone | merge 93 93 95 ms vs `e4b8ba4` 98 95 94 ms | `keylat.txt` |

**Boot time settled:** the kernel part (kernel entry to desktop) is 0.71-0.80 s on both. The whole spread is before the
kernel (firmware + loader): bimodal, 1.47-1.51 s or 1.77-2.15 s, on both images in the same session. The 2.56 vs 2.42 s of
11f-A is host noise in that phase, not a regression.

**Keylat:** smp 4 and smp 2 are equal to `e4b8ba4` within noise. Rounds 1-2 of smp 2 (165/173 vs 186/180 ms) ran while the
previous round's guests were still ending (the round wait matched an early FAIL line of the perf step); the three pairs
run alone afterwards are all 93-98 ms. Note: with the harness of this branch (`focus_omnibox`, fbread) `e4b8ba4` itself
measures ~95 ms, not the 128-149 ms 11f-A reported for it; the gain 11f-A reported is not visible in these pairs (both
sides use the same harness here). The perf step "reference site loads in under 3 s" fails on both (BUG-306, known).

## Open

- BUG-330 (LOW): one serial load may land out of order during an emergency flush while an AP sends; not reproduced.
- Keylat: why `e4b8ba4` measures ~95 ms with the merged harness (11f-A: 128-149 ms) is not explained; worth one pairing
  of the old and new harness on the same image.
- BUG-306, BUG-310 stay open (not part of this integration).
- No 2 h soak (left to validation).

## Host use

R630: CPUs 18-19 alone for the boot time; keylat on 0-11 with siblings 20-31 idle; regression: chrome 0-3, gui-smoke 4-7,
KVM boots 8-11, q35 16-17, TCG 12-15 + 32-37. No two measuring guests on SMT siblings of each other.
