# The arena of a browser process, the hang of s4, two hours at 4, 8 and 16 GiB (2026-11-14)

Branch `feature/chrome-mem2` from `feature/chrome-integ` `fd72e2c`. Not merged, not pushed.
BUG-156 and BUG-157 in `../../LOADTEST-TRACKER.md`; design `../../design/MEMORY-RECLAIM.md` section 7.

| Commit | What |
|---|---|
| `f2baa29`, `28d5d33` | BUG-157: the NMI probe allocates nothing, waits for no lock, prints the tasks without the scheduler lock, and is short |
| `c00776c` | `chrome-mem-soak.py`: `--local-video-dir` (the local VP9+Opus clip per round, sound checked), `--nmi-storm N` |
| `3966637` | BUG-156, the test first: `[memt] forkidle` (FAILED on that tree) |
| `8157a3d` | BUG-156: the arena of a fork child is made of the pages it uses |
| `b1dd150` | `fdpeers`: per child, who holds the other end of each of its sockets |
| `f08c678` | BUG-182: `[timer-probe] FROZEN` says which vectors are in service and when the timer last came |

Images: `m2-pub` (public, `ui.tour=off;update.check=off`), `t2-st` (self-test), `m2-plain` (public), all `f08c678`.
R630, KVM, `-cpu host`, CPUs 14-33, 2 vCPU per browser guest, user-mode network without `restrict` for the browser
sessions only, packs copied from `/srv/euroos/chrome-runs/`.

## 1. BUG-156: 128 MiB per process

Measured on the fixed image (4 GiB session, serial log): the browser forks with 23 MiB of its 128 MiB arena not zero
(`[fork] ... arena of 128 MiB: 23632 KiB copied, 107440 KiB of zero pages left out`), a child gives 17 MiB back at
its execve and holds 17 MiB afterwards (`[memstat] ... arena=17`). Before, every process held 128 MiB in one piece.

Test first (`memt-before-after.txt`, KVM):

| `[memt] forkidle` | Before (`3966637`) | After (`8157a3d`) |
|---|---|---|
| 4 GiB, 40 idle children at once | 16 made, the rest ENOMEM; 98 320 kB of process pool per child; FAILED 3/6 | 40 made; 4 616 kB per child; PASS 6/6 |
| 1 GiB, 20 children | 1 made; FAILED 3/6 | 20 made; PASS 6/6 |
| `[memt] memory self-test, 7 modes` | FAILED | OK at 1, 2, 4, 8 GiB (KVM) and in the four 1 GiB TCG configurations |

The fix and why it copies instead of sharing: design section 7. The launched process keeps its arena in one piece.

## 2. Two-hour sessions (`session-p*.txt`, `p*-curve.txt`, `p*.csv`, `processes-p*.txt`)

`chrome-mem-soak.py --no-video --free-video-seconds 0 --hours 2 --local-video-dir vid9 --local-seconds 60`: per round
the six sites of the matrix (euro-os, wikipedia, github, youtube front page, openstreetmap, the PDF), consent
banners answered by the harness, then the local clip `vp9.webm` (VP9 + Opus, `scripts/mk-test-video.sh`) from the
host at 10.0.2.2, 60 s, picture, clock and sound checked; `fdpeers` and a census after every round.

| Guest | rounds | steps ok / failed | PDF | clip with sound | in use first → last (min-max) | slope | lowest free of the demand pool | process pool, lowest free | refused forks, kills, aborts | processes |
|---|---|---|---|---|---|---|---|---|---|---|
| 4 GiB (p4) | 11 in 126 min | 179 / 0 | 11/11 | 11/11 | 714 → 690 (615-905) MiB | +16.0 MiB/h | 391 of 1 296 MiB | 1 451 of 1 668 MiB | 0, 0, 0 | 9-12 |
| 8 GiB (p8) | 11 in 125 min | 179 / 0 | 11/11 | 11/11 | 725 → 691 (562-866) MiB | +4.3 MiB/h | 2 366 of 3 232 MiB | 3 272 of 3 466 MiB | 0, 0, 0 | 8-12 |
| 16 GiB (p16) | 11 in 125 min | 179 / 0 | 11/11 | 11/11 | 713 → 688 (616-867) MiB | +8.7 MiB/h | 10 468 of 11 335 MiB | 3 254 of 3 466 MiB | 0, 0, 0 | 9-11 |

Before (`../chrome-mem/`): the PDF failed from round 1 at 4 GiB, sites failed after ~55 min at 8 and 16 GiB with
`[fork] arena alloc FAILED` 64-116 times per session, 27 processes.

- Sound: 12 of 12 five-second windows above the noise floor in every clip (RMS 1158), the clock of the video runs
  3.0 → 61.4 s in 60.7 s. Stereo Opus; 5.1 is BUG-181 (audio team).
- `unattributed` is 0 KiB at the end of each session and in all but 2 (p4), 6 (p8), 3 (p16) of ~325 censuses; the
  largest are 109 MiB (p4) and 2 MiB (p8, p16), a fork in progress whose child is not on the list yet. That one
  census is why `mem-curve.py` prints "NOT flat" for p4 (it wants under 1% of the pool without an owner at any
  time) beside a slope of +1.2% of the pool per hour; p8 and p16 print "flat".
- Kernel heap at 4 GiB: 117 → 143 MiB over the two hours (named RAM files, as in the first round); 117 → 121 at 8
  and 16 GiB.
- No `[oom]`, `[isolation]`, `[spawn-refused]`, `[timer-probe]` line and no NMI probe in any of the three.

## 3. Do idle utility processes end? (`processes-p*.txt`)

Per session ~130 forks (20 utility processes, 110 renderers): 44-49 children ended by `exit`, 72-78 by the
browser's SIGTERM, 9-12 alive at any census. At the end: the browser, `network`, `storage`, `on_device_model` and
the renderers of the open tab. `fdpeers` after every round: every stream socket of a child has its other end in
exactly one process (the browser, or the renderer/utility it talks to); no end is held twice, no reference is in
flight, none is at end-of-file unread. So in these sessions nothing blocks EOF/POLLHUP and no kernel bug was found
in CLOEXEC, fd inheritance or SCM_RIGHTS counting. The 17 `video_capture` utilities of the first round came with
the YouTube step; YouTube from the R630 still shows the bot check and is not part of these sessions, so that
process kind was not seen and whether it ends is not answered here.
(`fdpeers` prints "by another child too" where it means "by a child"; the wording is not corrected in `f08c678`.)

## 4. BUG-157: the hang of s4 (`s4-repeats.txt`, `nmi-storm.txt`)

The serial log of s4 (`/srv/euroos/mem-runs/s4/serial.log`, lines 7150-7179) shows the guest answering its console
(`cs-probe-746`, `-747`) until the harness sends the diagnostic NMI of `diag_stuck` (a navigation had not
happened); the probe prints six lines and the log ends. The hang began with the probe. `schedule_core+0x27c` with
IF=0 is where the NMI landed (the scan of the task table), and the "empty ready list" was a census that never ran.
After its sixth line the old probe cloned the task name into a `String` and later asked for the scheduler lock
that the interrupted code held; which of the two it stood in cannot be shown any more.

- The probe (`f2baa29`, `28d5d33`): no allocation, no lock that is waited for, the task table read without the
  scheduler lock; per task that could run its state, pid, cr3, vruntime, counts, last syscall and the rip/cs/IF it
  resumes with; the others as `tN:B/S/Z`. An example is at the end of `nmi-storm.txt`.
- `--nmi-storm 150` on a running 4 GiB guest, 3 sessions each: old probe (9 KiB a probe) 3/3 answer; first new
  census (25 KiB, 0.7 s in the NMI) 0/3, stopped by `[timer-probe] FROZEN` after 100-120 probes; short census
  (2.7 KiB) 3/3, 0 FROZEN.
- s4 repeated 20 times on `28d5d33` (integ + probe only; 4 GiB, matrix without video, two rounds, ~20 min each):
  0 hangs, 0 probes needed, 496 of 500 steps ok; the 4 failed steps (youtube 3, PDF 1) each have one
  `[fork] arena alloc FAILED` (BUG-156, not fixed on that image).
- Recorded as not reproduced, with this explanation and the slot fix of the integration (`3990f17`) as the
  other candidate.

## 5. BUG-182 (`[timer-probe] FROZEN`, audio team)

In the three runs of this team named in that entry (nsa1-3) the line follows `END NMI PROBE` each time: the long
probe of section 4 kept the timer away; fixed, 0 FROZEN lines since (nsc1-3, the 20 s4 sessions, the three
two-hour sessions). In the audio team's own run (av1, integ image) there is no NMI before the line and no
`[tick-late]` line names the end of the window: the 24 lines that message had were used up at line 759, the
window is at line 824. What the timer waited for there is not known. `f08c678` adds to the FROZEN line the whole
in-service register, the TSC since the last timer tick and the lost-tick total, and always logs a late tick of
200 ms or more. Not reproduced, not fixed.

## 6. Regression round, all on `f08c678`

| Check | Result |
|---|---|
| `cargo test` on the host (`cargo-test.txt`) | 187 test binaries, 1 363 passed, 0 failed (integ: 1 363) |
| KVM self-test boots 1, 2, 4, 8 GiB (`kvm-selftest-boots.txt`, `comparison-kvm.txt`) | desktop in all four; FAILED lines 2, 0, 0, 0 (integ: 15, 10, 0, 0); against integ `af6f415`: 0 worse, `churn-fork` FAIL → OK at 1 and 2 GiB, the `[fork] arena alloc FAILED` lines of the base (10 and 9) gone |
| TCG matrix of 10 (`tcg-summary.txt`, `tcg-*.markers`, `comparison-tcg.txt`) | 10/10 to the desktop, 0 watchdog trips, **0 worse in all 10**; the four 1 GiB configurations: `churn-fork` FAIL → OK, `[memt]` 7 modes OK |
| `gui-smoke.py` on the plain public image, KVM 4 GiB (`gui-smoke-public.txt`) | 67/67 |

## 7. Not done, open

- The pool split is unchanged: the process pool (1 668 MiB at 4 GiB) is at least 87% free in these sessions.
- Fork still copies (no copy-on-write): 23 MiB of arena and the demand pages of the browser per fork (56 + 33 ms).
- No reclaim for arena pages; a process pool that runs dry ends the process that touches a page (never reached).
- YouTube playback and the `video_capture` utilities (bot check); 5.1 sound (BUG-181); BUG-182.
- The R630 loadtest soak and the CI q35 matrix were not run.

## 8. Deviations

- The TCG matrix ran two boots at a time on CPUs 14, 15, 28 (smp4) and 29 (smp1), not in two queues of four.
- One KVM boot (t1-4g) shared CPUs 22-23 with an s4 session for four minutes (a wrong pin of mine).
- Commit `28d5d33` says 37 KiB per probe for the first census; measured afterwards it is 25 KiB.
- `userland/glibc/gmemt` and `gmemt.sig` are committed (the test binary and its signature, as the first round did).
- Screenshots and proof text were streamed through the laptop to the build server, not stored on it.
