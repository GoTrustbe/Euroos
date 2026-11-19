# BUG-400: the system clock runs slow against the host (11f-bug400)

Branch `feature/11f-bug400` from `19f7586` (`feature/11f-bug380`). Worktree `/opt/euroos/wt-11f-b400` on .16. Not pushed.
R630 run directory `/srv/euroos/b400-runs`, CPUs 18-19 and their siblings 38-39 only.

| commit | what |
|---|---|
| `475f73c` | tests first: `[clock-drift]` (the clock and the tick count against the HPET, at rest and with late ticks); `uptime` prints a `clock diag` line (HPET, TSC, tick count, timer interrupts, ticks they added) |
| `f487cc7` | the fix: `timebase.rs` (TSC time held to the HPET, else the ACPI PM timer); the tick count and the vDSO clock follow it |
| (docs) | tracker BUG-400 fixed, BUG-420 new; this directory |

## Cause (proven)

The clock was the tick count (`ticks x 10 ms`, BUG-355 design), and the tick count ran at the rate of the LAPIC timer.
The LAPIC period, and the TSC rate the kernel used, came from **one 10 ms window of PIT channel 2** at boot
(`apic::calibrate`): a few microseconds of port I/O at either end of 10 ms is hundreds to thousands of ppm, and nothing
corrected it later. Measured in the guest against the HPET (`[clock-drift]` diag, `tests-first.txt`) and on the host
over 30 min (`drift.txt`):

| | 10 ms calibration vs the TSC rate the HPET measures | timer interrupts vs the host | tick count vs the host |
|---|---|---|---|
| KVM, 7 base boots | **+747 to +1618 ppm** (period too long) | same as the calibration error | same |
| TCG, 2 base boots | +1618, +4672 ppm | | |
| KVM 30 min at rest (A), busy siblings (A), TCG (A) | +913, +2046, +2175 ppm | -855, -1975, -2222 ppm | -855, -1975, -2222 ppm |

At rest no interrupt was lost (`timer interrupts 1000 of 1000 the calibrated period predicts`): the whole drift was the
calibration. The tick count followed the interrupts exactly, and the vDSO clock followed the tick count (BUG-355).

Ruled out / smaller:
- **Lost or late ticks**: with interrupts held off 13-35 ms at a time (the `late` phase: 83 of 605 interrupts
  coalesced), the late-tick rule (`gap > 2 periods`) made up 84-86 of the 83 missing: under 2 ticks per 6 s. It misses a
  coalesced period when the tick before was itself late (gap under two periods), so it can lose under sustained delay,
  but at rest and in these runs it was not the cause. The fix makes it irrelevant (the count follows the time base).
- **The HPET or the host clock**: the HPET follows the host CLOCK_MONOTONIC to within a few ppm over 30 min (`drift.txt`,
  `HPET` column), so it is a sound reference; the guest TSC runs at the host TSC rate (2199.97 MHz, KVM and TCG).
- **The vDSO formula / BUG-380**: the vDSO clock followed the tick count to within 5 ppm before; it is the tick count
  that was slow.
- **Forced tick advances** (tickless idle, frozen-clock polls): 0 in every measured run (they only happen in the boot
  tests' glibc runs).
- **KVM vs TCG factor 10**: the calibration error is larger under TCG (port I/O of the PIT and port 0x61 is emulated
  in the TCG thread, so the polling loop lags more), and the TCG drift is -2222 ppm against -855 ppm at rest on KVM (results below).

## Fix (`f487cc7`)

- `kernel/src/timebase.rs` (new): the clock is `C(tsc) = N0 + (tsc - T0) x MULT`. It starts at the first timer tick at
  the tick count's time and the rate of the 10 ms calibration. About once a second the timer tick (BSP, interrupts off)
  samples the reference counter, the HPET (64- or 32-bit, wraps followed) or else the ACPI PM timer, between two TSC
  reads (the narrowest of three brackets; one wider than 200 us, a vCPU stopped in the middle, is tried again at the
  next tick). MULT is the TSC rate over the whole time since the reference was taken, minus the offset of the clock from
  the reference spread over 4 s (at most 500 ppm). The anchor moves to the sample, so `C` is continuous; the rate is
  always positive. Without HPET or PM timer the rate stays the calibration's (as before), with lost interrupts still
  made up.
- `schedule_tick` (`sched.rs`): each timer interrupt adds the 10 ms periods `C` passed since the last one (on a grid
  half a period off the interrupts, so jitter does not flip it): 1 as a rule, sometimes 0 (the LAPIC runs early),
  more after lost interrupts. Forced advances from task context add to the count as before, so the boot tests'
  tickless fast-forward is unchanged. The late-tick rule still logs `[tick-late]` and counts `LOST_TICKS`.
- `vdso_tick` (`ring3.rs`): the anchor target is `C(tsc)` plus the forced ticks (was `ticks x 10 ms`), the rate on the
  page is the time base's (was measured from one tick to the next). The BUG-355/380 rules are unchanged: the anchor is
  never below what readers saw, the sequence goes odd before the anchor TSC is read, interrupts off, the vDSO and the
  system call read one page with one formula (`vdso.so` not rebuilt: its formula did not change). A lead under 1 ms is
  now taken out over one period instead of running 7/8 for a period (that turned a 1 ns lead into a 1.25 ms lag and a
  jump forward at the next tick).
- `hpet::counter_mask` (32- or 64-bit counter).

What it does not change: the LAPIC period (still the 10 ms calibration: the scheduler's interrupt rate is as before),
`TSC_PER_TICK` (budgets, `[tick-late]`, klock), the BUG-355 slew for leads of 1 ms and more.

## Drift against the host, 30 minutes (`drift.txt`)
A = `475f73c` (the clock of `19f7586` plus the test and the diag line; the image was rebuilt on 2026-11-19 from that
commit and differs from `a-st.img` in 20 bytes of PE timestamp/debug hash only), B = `f487cc7`. A and B ran at the same
time on the same host. `uptime` over the serial console every 20 s, least-squares slope without the first sample
(that answer arrives 70-90 ms late, 40-50 ppm over 30 min; the HPET shows the same offset in the plain first-to-last
number). Goal: within +-50 ppm.

| 30 min | A (before) | B (after) | HPET in the same run |
|---|---|---|---|
| KVM at rest (2 vCPU) | -855 ppm (-1.5 s) | **+0.9 ppm** (first-to-last +52, HPET +42 the same way) | +0.8 |
| KVM at rest, repeat 2026-11-19 | | **-1.5 ppm** (first-to-last +28) | -1.4 |
| KVM, busy siblings (1 vCPU) | -1975 ppm (-3.5 s) | **+0.1 ppm** (first-to-last +16) | 0.0 |
| TCG (1 vCPU) | -2222 ppm (-3.9 s) | **+0.0 ppm** (first-to-last +2) | -0.1 |

KVM at rest and loaded are inside +-50 ppm by the slope in all three B runs. By the crude first-to-last number the first
B run is +52 ppm: that is the late first answer (the HPET reads +42 by the same method, the clock is 10 ppm from it).
TCG: the figure is **+0.0 ppm** (before -2222). 0 steps back in every run (`[vdso-mono]` OK everywhere, below).

## Test table
"Before" = run by the previous team on 2026-11-17, "now" = run by me on 2026-11-19. The image of every B test was
checked: `b-st.img` was rebuilt on 2026-11-19 from the clean worktree at `f487cc7` (same `SOURCE_DATE_EPOCH`) and is
byte for byte the same file (sha256 641b57aa...); `b-pub.img`, `b-br.img` were built from `f487cc7` after the commit.

| Test | Result | Who | File |
|---|---|---|---|
| tests first `[clock-drift]` on `475f73c` (before) / `f487cc7` | FAILED 9 of 9 (-726 to -5022 ppm) / OK 9 of 9 (clock 0 ppm) | before | `tests-first.txt` |
| cargo test (default members) | 188 suites, 1390 passed, 0 failed (log made after the commit) | before | log in `/opt/euroos/b400-img` (removed) |
| KVM boots 1/2/4 GiB x 1/2/4 vCPU, B and `19f7586`, markers-compare | 18 of 18 desktops, 0 crash lines, 0 worse in 9 of 9; FAILED lines the same set (3 at 1 GiB, 1 above), `[clock-drift]` OK and `[vdso-mono]` OK in all B boots | before | `kvm-markers-compare.txt` |
| 30 KVM boots with busy siblings (1/2/4 vCPU) | `[clock-drift]` OK 30 of 30, clock +0 ppm, `[vdso-mono]` OK 30 of 30, 0 crash | before | `kvm-30-loaded.txt` |
| TCG matrix 10 legs | 10 of 10 desktops, 0 watchdog, 0 NMI, `[vdso-mono]` and `[clock-drift]` OK per leg, FAILED lines per leg equal to 11f-bug380 (8/8/2/2/8/8/6/6/6/6, the known ones) | before | `tcg-matrix.txt` |
| drift 30 min KVM rest / KVM loaded / TCG, before and after | table above | before | `drift.txt` |
| drift 30 min KVM rest, repeat | -1.5 ppm | now | `drift.txt` |
| gui-smoke public image, KVM 4 vCPU | 67/67 | before | `gui-smoke.txt` |
| Chromium 20 starts | 20/20 | before | `chrome.txt` |
| Chromium one session, 3 rounds | 36/36, banners rejected (youtube) | before | `chrome.txt` |
| video VP9+Opus (local, clip map without `h264.mp4`: `opus.webm`, `vp9.webm`, `vp9v.webm`) | picture advances in 28 of 28 screendumps per minute, `currentTime` advances about 58.5 s in 60.5-60.8 s of wall clock (picture and sound stay level), sound in 12 of 12 windows. B and base side by side: 75/75 and 75/75. First run of B 73/75: the two failures are openstreetmap (BUG-341) | before | `chrome.txt` |
| loadtest soak 1 h, B (the earlier run was stopped after 17 min, no result) | completed clean: 5925 commands (5461 an hour), 0 not ok, hammer 344 032 requests 0 errors p50 7.0 ms, churn fs median 8.6 s max 10.4 s, churn proc max 2.6 s, 0 crash/NMI/watchdog/deadman, order inversions 0 | now | `soak-sched-2026-11-19.txt` |
| loadtest soak 1 h, base `19f7586`, side by side | completed clean: 5958 commands (5493 an hour), 0 not ok, hammer 342 959 requests 0 errors p50 7.0 ms, churn fs 8.6 s max 10.1 s, churn proc max 3.2 s | now | same |
| scheduler `[fair]` hogtest 5 10 8, B and base, 2 runs each | B worst 0.4 s, lowest share 62-63 %; base worst 0.4-0.5 s, 61-62 % (limit 12.6 s), all OK | before | same |
| scheduler: the driver budget (BUG-252, `sched.drvbudget`) | covered by the soak above (hammer and churn under the 10 % of 500 ms budget): B equal to base | now | same |
| typing delay (fbread, 30 keys) and site load, 5 pairs B and base | key to screen median B 67, 65, 62, 68, 68 ms; base 67, 68, 67, 65, 61 ms (no difference). Site load under 3 s: B failed once (3.04 s), base failed once (3.16 s), the other 8 runs 2.46-2.93 s: weather of the real site, equal on both sides | before (pair 1, 2), now (3 rounds) | same |

## What sets the wall clock, and does anything adjust it later
Read from the code (`ring3.rs`, `rtc.rs`), unchanged by this fix: REALTIME is set once, when the first process
sets up the vDSO page (`vdso_prepare`): the CMOS RTC (whole seconds, the fraction is dropped, so the start is 0 to 1 s
behind the host; under QEMU the RTC is the UTC time of the host) becomes the base. After that REALTIME is that base plus
the monotonic clock of the time base. Nothing adjusts it later: no NTP/NTS client in the running system, no
`clock_settime`/`settimeofday`/`adjtimex` in the kernel, no second RTC read for the wall clock. So a host clock change
after boot is not followed; the rate is right (BUG-400), the offset is not corrected (BUG-420). In the measured runs
host wall minus host monotonic stayed 0.000 s, so the comparison above holds for REALTIME as well.

## Not run / deviations
- Not run: nothing from the list is missing. What is marked "before" was not repeated; I checked it by reading its
  logs and by rebuilding the B and A images.
- A is `475f73c`, not the bare `19f7586`: the same clock code plus the test and the diag line the measurement needs.
- The previous team ran its tests on CPUs 18,19,38,39; my runs used 0-9 and 20-29 only.
- The R630 copy of `base-br.img` was shorter than the one on .16 (178 MB, different hash); it was replaced by a
  checked copy (sha256 176ea264...). The earlier keylat and video runs of the base with the short file passed; the three
  new keylat rounds use the replaced file.
- First-to-last drift of the first B rest run is +52 ppm (late first answer, see above); the slope is +0.9 and the repeat
  -1.5 (first-to-last +28).
- `[aslr-ind]`, `[conn]`, GBIG and the `[udrv-*]` FAILED lines are the known ones, equal on `19f7586`.
- Open from this work: BUG-420 (wall clock offset never adjusted), BUG-421 (markers-compare misses "-> OK"), see the
  tracker. The LAPIC period is still the 10 ms calibration (the interrupt rate is as before: 99.62 to 99.92 Hz); only the
  tick count and the clock follow the time base.
