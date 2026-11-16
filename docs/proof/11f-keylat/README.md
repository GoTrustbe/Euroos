# Key latency in Chromium (sprint 11f-A, feature/11f-keylat, 2026-11-16)

Branch `feature/11f-keylat` from `e4b8ba4`, worktree `/opt/euroos/wt-11f-a` on .16. Not pushed. R630 run directory
`/srv/euroos/11fa-runs`, CPUs 4-11 only, their SMT siblings 24-31 left idle, work directories on tmpfs (`/dev/shm/11fa`).
Browser images = public image with `ui.tour=off;update.check=off`; `smpuser` stays off. Reference: `e4b8ba4` built here
(browser `b0-br`, self-test `b0-st`); its public image is the one of the same code (`732e5f5`, `val-runs/img/v2-pub.img`).

## 1. Result

Final code `544d5a2` (+ `4e681ba`, which only fixes the wait of the `serialcost` test command). Pairs started together:

| keylat, median key to screen | `e4b8ba4` | final `544d5a2` |
|---|---|---|
| smp 4, new method (fbread), 3 pairs | 149 134 128 ms | **96 96 99 ms** |
| smp 2, new method (fbread), 4 pairs (+1 single run) | 148 149 147 139 ms | **93 92 83 90 (91) ms** |
| smp 4, old method (screendump), same runs | 247 249 223 ms | 167 163 154 ms |
| smp 2, old method (screendump), same runs | 222 223 225 239 ms | 141 150 151 157 (138) ms |
| kernel, key make code to first paint (fbread half) | 97-111 ms | 71-79 ms |

Target < 100 ms with `smpuser` off: **met with the new method**, every pair faster (83-99 ms against 128-149 ms). With the
old method the same runs read 138-167 ms (was 222-249), because that method slows the guest it measures (section 2). The
same pairs on `049ed8f` (before `eee05a6`/`544d5a2`): smp 4 94 94 96 against 144 156 141, smp 2 97 88 82 90 against 142 149 142
145. Pairs: `final-keylat.txt`; every build of the sprint: `keylat-steps.txt`.

## 2. Measurement first: the harness's own share

The perf step polled QMP `screendump` (the whole 1920x1080 screen, 18-27 ms a poll) until the field changed. Per key the
harness now also records when the screendump that showed the change returned and the one before it (the "bracket"): the
polling quantisation itself is only 17-27 ms, and the digest after it ~3 ms. The large share is elsewhere: a screendump holds
QEMU's big lock for its copy, and every guest exit that needs it (the xHCI registers the desktop loop polls every pass, the
UART, the HPET) waits for it. The same keys in the same boot, typed twice (`2924d28`):

| same boot | screendump (old) | fbread (new: QMP `pmemsave` of the field rows of the GOP framebuffer, ~3 ms a poll) |
|---|---|---|
| `e4b8ba4` + measurement, smp 4 (`m1b-4`): harness median | 273 ms | 156 ms |
| desktop loop on the CPU per pass | 3.0 ms ("devices and windows" 1.6 s in 30 keys) | 0.86 ms (0.19 s) |
| kernel `[keylat]` total | 184 ms | 105 ms |

fbread is within 1-4 ms of the kernel's paint of the frame that carries the character (`keylat-per-key.txt`: harness
142/133/151 ms against paints at 138/127/146 ms). Both methods stay in the step; the verdict uses fbread (BUG-300). The
field is emptied between them: a full field scrolls by an identical glyph and only the caret's blink changed it (~650 ms,
BUG-305).

## 3. The key-to-pixel path (smp 4, fbread, medians; before = `e4b8ba4` / m2, after = m8 and `k1-f-4`; the final `kf3` runs give the same kernel stages, 71-79 ms to first paint)

| stage | before | after | how measured |
|---|---|---|---|
| key IRQ (make code) to the X client's queue | 0.12 ms | 0.12 ms | `[keylat]` ring to X client |
| X queue to the client's read | 3.8-6.6 ms | 0.2 ms | `[keylat]`; the reader waited in poll/epoll for the tick |
| client read to its first draw (chrome) | 84-108 ms | 74 ms | `[keylat]` |
| frames: page area 715x256 (3 PutImage), then the text line (Nx21) | ~100 and ~160 ms | ~60-80 and ~70-100 ms | `[keylat-ev]` |
| last frame to its paint (composite + blit) | 5-200 ms (composite 5-10 ms) | 1-3 ms (composite 0.46-0.56 ms) | `[keylat-ev]`, `[keylat] composite` |
| delivery to the paint of the character: desktop loop / other tasks / idle | 21 / 124 / 31 ms (172 ms) | 0-1 / 84-88 / 0-3 ms (88-93 ms) | `[keylat-cpu]` ledger per key |
| of "other tasks": the console sender (t4) | 18-115 ms in a key | 0 | ledger top tasks |
| BSP idle that only the tick ended while a key was on its way | 39-52 ms a key | 0-0.4 ms a key | `[keylat] BSP idle` |
| paint to the harness seeing it | fbread +1-3 ms; screendump +100-130 ms | fbread +1-3 ms; screendump +65-75 ms | harness capture against paint |

What is left (88-93 ms from delivery to the character's paint) is chrome's own work on the BSP: idle 0-3 ms, desktop loop
0-1 ms, chrome threads 84-88 ms (renderer main ~20, browser main ~17, compositor ~18, ...). See section 6.

### Why the `[desk]` passes of 100 ms and more (fix-d2)

Not a tick wait. A pass's wall time counts the turns of the chrome threads that preempt it (the loop runs at nice 0 while it
hosts chrome). Its own CPU (`[desk-cpu]`, new) was 3.0-3.2 ms a pass under the old harness: "devices and windows" (xHCI
register reads stalled behind QEMU's lock during screendumps), "hosted window and console" (the console kick: up to 256 bytes
at ~31 us a byte) and "drawing" (the whole 800x600 X body composited with one `put_pixel` per pixel, 5-10 ms). After the
fixes: 0.26-0.31 ms a pass with fbread (1.7 ms under screendump polling: the lock wait).

## 4. Fixes, in the order of their size (test first; commits)

| # | commit | what | test (fails before, passes after) | measured |
|---|---|---|---|---|
| 0 | `2924d28`, `5fb4537`, `90f7ee6` | measurement: `[keylat-ev]` (every frame with its size, every paint), `[keylat-cpu]` ledgers, `[desk-cpu]`, BSP idle by what ended it, tick wakes by system call; harness fbread, sched-stat around the keys | - | sections 2, 3 |
| 1 | `763a9bc`, `eee05a6` (tests `90f7ee6`, `049ed8f`) | epoll_wait/poll waiters are woken when an eventfd is written, a unix stream gets data or is closed, or the desktop loop queued X events; per-call epoll deadline (BUG-303, BUG-304) | `[keylat] BSP idle ... ended by the timer tick`: 39-52 ms a key FAILED -> 0-0.4 ms OK (limit 5) | fbread 133-147 -> 82-97 ms |
| 2 | `a344592`, `e33ac3f` (test `89d4cba`, wait fixed in `4e681ba`) | the console queue is sent by a kwork job on an AP (BUG-301) | `[serial-cost]` 8 KiB: 249-254 ms of BSP FAILED -> 0 ms OK | fbread 156-178 -> 145-147 ms |
| 4 | `544d5a2` | private futexes keyed by address space, not by address alone (BUG-307; a hang the faster wakeups of fix 1 exposed) | no isolated marker; chrome-smoke: start 19/20 and interaction 5/18 before, 20/20 and 18/18 after | - |
| 3 | `379fd72` (test `5303e44`) | the hosted X window: only the rows a PutImage/CopyArea changed are composited and blitted, by row copies (BUG-302) | `[keylat] composite`: 5053-10102 us FAILED -> 464-561 us OK (limit 1500) | fbread 145-147 -> 133-144 ms; loop 0.44 -> 0.30 ms a pass |
| - | `841ab78`, `32b519e`, `42ce6a3` | harness: field emptied between methods; sched-stat; `--perf-smpuser-ab` | - | - |

Fix 1 had a hole in its first form (`unblock_any`): it could end an unrelated Blocked wait of the same task, and chrome's
navigation stalled in 2 of 3 smp 4 runs (`m7-4`, `m7b-4` in `keylat-steps.txt`). The wake now only ends the waiter's own
timed sleep, checked under the scheduler lock (`sched::wake_sleeping`). `eee05a6` closed two more holes (a deadline re-armed
by event wakes; a killed thread's registration in a reused slot). The hang still came back (start 3/20 on `049ed8f`,
interaction 5/18 on `eee05a6`): a renderer's main thread Blocked on a futex in the chrome image whose word was already 0. The
cause was older than this branch: the futex queue matched waiters by user address alone, and every chrome process maps its
binary at the same address, so a wake in one process could take the waiter of another (BUG-307, fixed in `544d5a2`).
`[fair]`, `[kwork]`, `[fuzz]` and the loadtest limits are not touched by any of these (`[fair]` 61 %, section 5).

## 5. Final regression

On `544d5a2` unless noted; `4e681ba` changes only the `serialcost` shell command and was checked with that command alone.

| Test | Result |
|---|---|
| `cargo test` (default members, as gate.sh) | 188 binaries, **1 381 passed, 0 failed**, 1 ignored (`cargo-test.txt`), as `e4b8ba4` |
| KVM self-test boots 1/2/4/8 GiB, 2 vCPU, HD-Audio (`final-kvm.txt`) | desktop 4/4, 0 crash lines; FAILED lines 3/1/1/1 as on `e4b8ba4` (`[aslr]` BUG-088 and `GBIG` at 1 GiB); markers-compare **0 worse** at all four sizes; `[kwork]` OK, `[fuzz]` OK |
| `[fair]` hogtest 5 + churn 8, public image, 4 vCPU (`final-boot-fair.txt`) | lowest task 0 share **61 % OK** (`e4b8ba4` in the same hour: 50 %, its known 47-52 % band) |
| `[serial-cost]` (`serial-cost.txt`) | smp 4 / 2: BSP 0 ms for 8 KiB, OK; smp 1: SKIP (BSP 325 ms, no AP) |
| gui-smoke, public image, 4 vCPU 4 GiB (`final-chrome-gui.txt`) | **PASS 67/67** (also on `049ed8f`) |
| Chromium 20 starts, smp 2 (`final-starts.txt`) | **20/20, 0 FROZEN**; `e4b8ba4` beside it 20/20; `049ed8f` 19/20 (start 3: BUG-307) |
| sites, 3 rounds, smp 2 (`final-sites.txt`) | **36/36**, 0 crash lines, 0 FROZEN (also 36/36 on `049ed8f`) |
| interaction, smp 2 (`final-chrome-gui.txt`) | **18/18** (also on `049ed8f`; `eee05a6` 5/18: BUG-307) |
| keylat pairs, smp 4 and smp 2 (`final-keylat.txt`) | section 1: 3 + 4 pairs, all faster, 0 omnibox/navigation misses |
| boot time, public, 2 vCPU 2 GiB, 10 alternating pairs (`final-boot-fair.txt`) | median **2.56 s** (2.28-2.78), limit 2.8 s; `e4b8ba4` 2.42 s; kernel entry to desktop is the same (0.775 against 0.770 s), the difference is before the kernel runs (firmware, loader, a host shared with other teams' TCG guests) |

## 6. Is what is left CPU contention that S7 would remove? (no S7 built)

- After the fixes the BSP is busy for the whole key: from delivery to the character's paint 88-93 ms, of it chrome threads
  84-88 ms, idle 0-3 ms, desktop loop 0-1 ms (`keylat-steps.txt`, `keylat-per-key.txt`). The latency is chrome's CPU time on
  one CPU; with `smpuser` off nothing else is left to remove in the kernel's part of the path.
- Over the 30 fbread keys (`smpuser-ab.txt`, 10.7 s): chrome threads ran 4.4 s, 25-30 % of it in system calls and demand
  faults; the busiest threads make 450-1260 system calls a second, one every ~0.8-1.1 ms of their run time.
- `smpuser on` (S5) in the same boot: 97 ms against 94 ms off. The APs ran 42 + 29 ms of ring-3 time in 10.8 s; all 1 809
  offloads were sent home, 1 768 of them by a system call (`[smpstat]`). A thread does not stay on an AP long enough to
  matter: its next system call is about a millisecond away.
- S7 moves only `futex_wait`/`futex_wake` to the APs. The key path's threads mostly wait in epoll_wait/poll and talk over
  unix sockets and eventfds (the tick wakes before fix 1: epoll 5 363-8 498, poll 906-1 475, futex 1-2). With S7 alone most
  of their system calls would still bounce home.
- Estimate (not measured): even if chrome's key work spread over 4 CPUs, the critical path stays the chain browser main ->
  renderer main -> compositor -> viz (13-24 ms each in the ledger); a realistic gain is ~20-40 ms of the ~90 ms, and it needs
  epoll/poll/sendmsg/recvmsg/eventfd on the APs, not only futex. Recommendation: no S7 for keylat now; if more is wanted,
  first count the `smpstat` bounces per system call number.

## 7. Not done / notes

- No 2 h soaks (validation team).
- Descriptors without a wake key (pipes, inet sockets) keep the 1-2 tick re-check in epoll_wait/poll: 12-17 late finds a
  key, none on the key path (BUG-303 "Left").
- "perf: reference site loads in under 3 s" fails on `e4b8ba4` (3.7-4.6 s) and on this branch (4.0-4.6 s): BUG-306, not
  investigated.
- The kernel's `[keylat]` total stops at the first paint after the client's first draw; the character can come with a later
  frame (`[keylat-ev]` lists them all). fbread measures the character itself.
- `keylat` prints more per key now (`[keylat-ev]`, `[keylat-cpu]`); with APs that output costs the BSP nothing (fix 2); on
  smp 1 it does.
- The R630 was shared with other teams' TCG runs (CPUs outside 4-11) during the regression; the boot-time numbers move with it.
- R630 drivers: `drivers/` (`cms.sh`, `klat.sh`, `wave2-4.sh`, `scmd.py`); `kboot-hda.sh` is the one of `../revalidate-2026-11e/drivers/`.
