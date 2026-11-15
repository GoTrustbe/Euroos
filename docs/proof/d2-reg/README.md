# D2-reg: the three regressions of D2 (BUG-250, BUG-251, BUG-252), 2026-11-15

Branch `feature/d2-reg` from `feature/d2` `2cf4320`, worktree `/opt/euroos/wt-reg` on .16, not pushed. R630 run directory
`/srv/euroos/reg-runs` (CPUs 0-39). No new features, no S7, `smpuser` stays off by default. Final images: tag `r2` = `72976f7`.

| Commit | What |
|---|---|
| `8a6f333` | BUG-250, the test first: `[root-heap]` counts the RAM-root backing alive in the kernel heap (FAILED on 2cf4320) |
| `c70dddf` | BUG-250: a RAM root is moved into the block cache, not cloned |
| `0cf75e6` | BUG-251: the serial sender never polls with interrupts off, port accesses outside the UART lock, every print deferred; (a chrome low-end flag, removed again in `72976f7`) |
| `f42be2a` | BUG-252: a userspace driver has nice -15 within a budget of 10 % of every 500 ms |
| `72976f7` | BUG-251: the low-end flag removed; the keylat gap was the host's disk |

## BUG-250: 1 GiB storage configurations, 8 MiB of kernel memory (`bug250-storage.txt`)
Cause: BUG-220 (small) kept the root device after the format for its RAM-root fallback and gave the block cache a `clone()`
of it; for a RAM root that is a second 8 MiB for the whole boot. Fix: `RootBlk::for_cache` moves a RAM root, copies a disk.

| 1 GiB, `storage.nvme=user`, IOMMU | 2cf4320 + test (`8a6f333`) | fix (`c70dddf`, final `r2`) |
|---|---|---|
| `[root-heap]` (KVM) | 16 384 KiB alive, FAILED | 8 192 KiB, OK |
| kernel memory left at the `[ramfiles]` limit line | 35 MiB (sched-d1: 45) | 43 MiB (KVM and all four TCG storage/combined configs) |
| `[rename]` / `[rename-disk]` / `[app-policy]` | FAILED / FAILED / FAILED | OK / OK / OK (KVM; TCG storage and combined, smp 1 and 4) |
| TCG matrix against validate-2026-11d | 3 worse in the 1 GiB storage configs (d2) | **0 worse in all 10** |

## BUG-251: keylat after the integration (`bug251-bisect.txt`, `keylat-final.txt`)
Bisect over the merges (images at each merge, chrome-smoke `--only perf`): sched-d1 475-547, + small 469, + kbugs 337, + mem
566-651 ms; `b6eb05d` with only `4c0467d` (BUG-240, true sysinfo) reverted 315-458 ms. But all these runs had their work
directories on the R630's root disk next to soaks: 126 MB/s of writes, IO pressure 10-17 %. QEMU writes each screendump
(6 MB, one per poll of the harness) in its main thread with the device lock held, so every UART exit of the guest and the
timed screendump wait for the disk; the same image then measures 330 ms or 1 083 ms. With the work directories on tmpfs
(`cms.sh`) the integration is inside the sched-d1 spread, and a chrome low-end flag (tried in `0cf75e6`) changes nothing,
so it was removed (`72976f7`) and chrome keeps the sizing BUG-240 intended.

The guest made the host stalls worse: the serial sender polled the line status with interrupts off until the transmitter
emptied, a print of any other task or of the timer interrupt did the same, and a print waited for the UART lock while its
holder sat in a slow exit (`UART lock held ... through 200M spins`, IF=0 windows up to 9 s, on sched-d1 images too). Now
the sender reads the status once and sleeps a tick when busy, a load is one string output of at most 16 bytes, the port is
accessed outside the lock, and every print defers once the desktop loop runs.

Final, tmpfs work directories, four runs at a time, smpuser off (`keylat-final.txt`, rounds kf1/kf2):

| key to screen, median | smp 4 | smp 2 | t4 share | IF=0 windows of t4 (sum) |
|---|---|---|---|---|
| sched-d1 `be87a0c` | 367, 386 ms | 372, 363 ms | 8.4-12.2 % | 329-825 ms per run (all causes) |
| integration `b6eb05d` | 381, 383 ms | 403, 381 ms | 8.1-12.3 % | |
| final `r2` | **390, 396 ms** | **372, 360 ms** | **6.7-11.7 %** | **0** (no window over the detector's limit) |
| on the disk, with soaks beside them | d1 823-1 083 ms | d1 579-1 007 ms | | |

`[serial-tx]` of `r2` after `chatter on` + `dmesg` + `ramfiles`: transmitter busy 0 times, longest load with interrupts
off 519 us. Not met: the absolute 350 ms; sched-d1 itself measures 330-455 ms on this host today (342 ms in d2's run).

## BUG-252: console commands under the hammer (`bug252-soaks.txt`)
Cause: e1000d under the hammer is no short pass per interrupt; each frame is a device exit that under QEMU's user network
carries the host's TCP work (about 1 ms of CPU per request); at the fixed weight of -15 it took 45 % of the CPU. Fix: nice
-15 within 10 % of every 500 ms, nice 19 past it, no vruntime debt into the next window (`sched.drvbudget=`,
`sched.drvwindow=`, `drvbudget`).

| 25-minute soaks (same scenario as the 2 h soak) | cmds/h | churn fs median / max | hammer p50 | deadman |
|---|---|---|---|---|
| 2cf4320 (fixed -15) | 2 812 | 18.9 / 57.6 s | 5.0 ms | 1 |
| 10 % of 100 ms | 5 110 | 9.4 / 11.1 s | 8.9 ms | 0 |
| 30 % of 100 ms | 4 006 | 12.9 / 14.8 s | 4.9 ms | 1 |
| 10 % of 1 s | 5 055 | 9.1 / 10.2 s | 5.1 ms | 0 |
| **10 % of 500 ms (chosen)** | **5 232** | **8.9 / 10.6 s** | **5.7 ms** | **0** |

2 h soak of `r2`: 5 217 commands an hour, churn fs worst 10.6 s, hammer p50 5.6 ms, 0 deadman trips (`final/`).
D1 criteria on `r2` (sched.py, KVM 4 vCPU): `[fair]` 30 rounds worst 2.4 s (limit 12.6 s; the share verdict FAILED as on
sched-d1 and d2), `[wakelat]` p50 0.07 ms.

## Final regression of `r2` (`final/`)
| Test | Result |
|---|---|
| cargo test | 188 binaries, 1 381 passed, 0 failed, 1 ignored |
| KVM self-test boots 1/2/4/8 GiB | 4/4 desktops, 0 crash lines, 0 inversions; FAILED lines `[aslr]` + GBIG at 1 GiB (as d2); markers-compare **0 worse** against validate-2026-11d and d2 |
| TCG matrix 10 | **10/10 desktops, 0 watchdog, 0 NMI, 0 inversions, 0 worse in all 10** |
| gui-smoke public | **67/67**, markers-compare 0 worse |
| Chromium smp 2 | sites 3 rounds **36/36**, interaction **18/18**, local VP9+Opus picture and sound OK (H.264/AAC fails by design, 16/21), keylat above |
| soak 2 h | **5 217 commands an hour** (d2: 2 887), 10 866 ok, 0 errors; churn fs median 8.3 s **worst 10.6 s** (d2: 18.2 / 29.5 s); churn proc median 1.5 s worst 4.0 s (limit 12.6); hammer **p50 5.6 ms**, 774 020 requests, 0 errors; **0 deadman trips**, 0 panic/NMI/FROZEN/inversions, 0 "200M spins" |

## Not done / open
- keylat's absolute 350 ms: `r2` is at the sched-d1 level of this host (360-396 against 363-386 ms), not under 350.
- The R630 harness writes screendumps to its work directory; on a busy disk keylat numbers are not comparable. `cms.sh` puts
  the work directory on tmpfs; `scripts/chrome-smoke.py` itself is unchanged.
- The driver budget is per window and per driver; a second busy driver (nvmed under a disk flood) has its own budget.
