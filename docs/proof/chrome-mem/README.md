# Where the memory of a browser session goes, and getting it back (2026-11-13)

Branch `feature/chrome-mem` from `feature/chrome-fdspace` `40f853a`. Not merged, not pushed.
Bugs BUG-150 to BUG-156 in `../../LOADTEST-TRACKER.md`; design `../../design/MEMORY-RECLAIM.md`.

| Commit | What |
|---|---|
| `66312a3` | the census: `[mem]`/`[memstat]` per owner every 30 s and after `ramfiles`, `memstat` in the shell, `scripts/mem-curve.py` |
| `8f88f1b` | the tests first: `[memt]` (`gmemt` forkfail, madvise, memfd, oom). FAILED in all modes on that tree |
| `48bf2e7` | BUG-150 (a refused fork gives back what it took), BUG-151 (task slots of ended processes; 512 tasks, 448 thread stacks) |
| `b882f4d` | BUG-152 (`madvise(MADV_DONTNEED/MADV_FREE)` gives the frames back) |
| `15fe72b` | BUG-153 (page-cache eviction), BUG-155 (out of memory: reclaim, then one child, never the kernel) |
| `f9a8870` | BUG-154 (memory files made of frames, budget from the pool) |
| `bddf3a5`, `f0f2f59` | the design; `scripts/chrome-mem-soak.py` (site matrix + video per round, on top of `chrome-smoke.py`) |
| `1dd0d58` | `[memt]` on the 96 MiB arena (it could not fork on 1 GiB) |

Browser sessions ran on the image of `f9a8870` (public image, `ui.tour=off;update.check=off`, sha256
`57a470ff09d37ad1…`); the self-test image of `1dd0d58` (`1dd0d58` changes the self-test only). R630, KVM,
`-cpu host`, CPUs 14-27, 2 vCPU per browser guest, user-mode network without `restrict` for the browser
sessions only, `chrome-pack2.img` and `nss-pack-vid.img` copied from `/srv/euroos/chrome-runs/`.

## 1. The question: how can memory fill up through YouTube when nothing is playing?

Measured first, on the tree before any fix (image of `66312a3` plus the task counters; runs b8, b8n, c8 at
8 GiB, c4 at 4 GiB; `before-*-curve.txt`, `before-*.csv`). The census names the owner of every frame of the
demand pool; `unattributed` is what has none.

Run c8, 8 GiB, pool 3 237 MiB, MiB after each site (`used` = pool minus free):

| min | round, site | used | anonymous | of it all zeros | page cache | memory files | processes |
|---|---|---|---|---|---|---|---|
| 0 | 1 euro-os | 439 | 94 | 17 | 212 | 36 | 6 |
| 1 | 1 github | 635 | 252 | 63 | 224 | 44 | 7 |
| 2 | 1 youtube | 835 | 395 | 94 | 227 | 51 | 10 |
| 3 | 1 pdf | 818 | 355 | 115 | 231 | 38 | 12 |
| 13 | 2 euro-os (FAIL) | 1 879 | 102 | 40 | 233 | 9 | 5 |
| 19 | 2 pdf (FAIL) | 2 783 | 103 | 41 | 233 | 8 | 5 |
| 59 | 4 pdf (FAIL) | 3 135 | 69 | 28 | 233 | 7 | 1 |
| 83 | 5 pdf (FAIL) | 3 209 | 69 | 28 | 233 | 7 | 1 |

`unattributed` in the same run: 0 KiB at minute 0, 825 604 at minute 7, 1 960 320 at 16, 2 413 464 at 24,
2 799 008 from minute 50 on; pool free 2 805 → 0 MiB. So:

- **It is not the page cache** (212-235 MiB, flat) **and not the memory files** (7-51 MiB of frames, 25-126 MiB
  of bytes in the kernel heap).
- **It is not what the pages hold.** After the first round the sites are not even loaded any more (every step
  FAILs): the tab sits on one page, "nothing is playing", and the pool still drains.
- **2.8 GiB belong to nobody.** `[spawn-refused] fork: the task table is full (thread kernel stacks 131/224,
  tasks 135/256)`: the table of 256 tasks was used up by the slots of ended tasks (BUG-151), every fork of a
  renderer was refused, and every refused fork kept the arena (128 MiB), the page tables and the copy of the
  demand pages (~105 MiB) it had taken (BUG-150). The browser retries for every navigation and every timer of
  its own. The same refusals are why a new renderer aborted at its start after the PDF and why typed URLs went
  nowhere (BUG-134 and BUG-135 as seen in these runs).
- When the pool is empty the next page fault of whoever faults fails: `[isolation] … (task 1) TERMINATED`, the
  browser's main thread, at minute 59 in b8n and at minute 15 in c4 (4 GiB, pool 1 300 MiB) (BUG-155).
- Second, smaller: **25-40% of the anonymous pages were all zeros** (`zero~`): given back by the program with
  `madvise` and kept by the kernel (BUG-152).
- V8 and PartitionAlloc reservations (370 GiB of addresses per renderer, 16 GiB of them PROT_NONE) cost page
  tables only: 4-20 MiB per session. They grew to 148 MiB in the dead browser of c8 (tables are not freed
  before the process ends); with the fixes they stay at 16-27 MiB over two hours.

## 2. Tests first, then the fixes

`[memt]` runs `userland/glibc/gmemt.c` in a demand-paged process; it reads the kernel's account from
`/proc/euroos/mem`. KVM, 4 GiB (`memt-before.txt`, `memt-after.txt`):

| Mode | Before (tree of `8f88f1b`, forkmany added) | After |
|---|---|---|
| forkfail | FAILED 1/4: process pool 1 708 032 → 528 336 kB free over three refused forks (3 × 384 MiB), demand pool −24 648 kB, still lost after the run; the next fork refused | PASS 4/4, 0 kB lost |
| forkmany (270 children, every second one ended by SIGTERM) | FAILED: 0 made (the pool was gone after forkfail) | PASS 3/3 |
| madvise | FAILED 4/6: 0 kB of 32 MiB came back | PASS 6/6 |
| memfd | FAILED 4/12: 16 MiB of file = 16 MiB of kernel heap plus 16 MiB of frames once mapped; a store through a mapping not read by `pread`; no limit (65 536 kB written, no ENOSPC) | PASS 12/12: 0 kB of heap, one memory, ENOSPC at the budget |
| oom | FAILED (no fork) / exit 139 | PASS 5/5: the child that holds the pool is ended (SIGKILL), the main process gets its memory |

After, KVM at 1, 2, 4 and 8 GiB: `[memt] memory self-test, 5 modes → OK ✓` in each (`kvm-selftest-boots.txt`).
Host tests of the models: `euromm` 38 tests (`ramfile` against a byte vector with a frame source that runs out,
`pagecache` against a brute-force list, `oom`), `eurofdspace` 20.

## 3. Two-hour browser sessions on the fixed kernel

`scripts/chrome-mem-soak.py`: per round the six sites of the matrix, then YouTube (front page, watch page,
consent dialog, a click on the player, 120 s), then a public video file in the browser's own player (120 s).
`after-p4-curve.txt`, `after-p8-curve.txt`, `after-p16-curve.txt` and the `.csv` time series.

| Guest | demand pool | budget of memory files (pool/2) | rounds | in use first → last (min-max) after 15 min | slope | lowest free | kill lines | unattributed at the end |
|---|---|---|---|---|---|---|---|---|
| 4 GiB (p4) | 1 296 MiB | 648 MiB | 12 in 123 min | 704 → 658 (524-842) MiB | −47 MiB/h | 416 MiB | 0 | 0 KiB |
| 8 GiB (p8) | 3 232 MiB | 1 616 MiB | 13 in 130 min | 990 → 1 084 (851-1 262) MiB | −10 MiB/h | 1 970 MiB | 0 | 0 KiB |
| 16 GiB (p16) | 11 335 MiB | 5 667 MiB | 12 in 122 min | 997 → 895 (810-1 243) MiB | −42 MiB/h | 10 092 MiB | 0 | 0 KiB |

No browser was ended, no `[oom]` line, no `[spawn-refused]` line, no `POOL EXHAUSTED`, in any of the three.
Per owner over the two hours (16 GiB): anonymous 145-494 MiB (slope −82 MiB/h), of it all zeros 2-13 MiB
(before: 94-115), page tables 16-26, page cache 252-257, memory-file frames 15-69, kernel heap 123-128 MiB.
`unattributed` is 0 in 308 of 337 censuses and 39 MiB at most in the others (a fork in progress or one that was
just refused and given back); mem-curve.py calls a run "flat" only under 1% of the pool, which is why p4 (68 MiB
of 1 296) and p8 (121 of 3 232) print "NOT flat" with a falling slope.

What a site adds to the memory in use (16 GiB, MiB, first round / mean of the later ones): euro-os 0 / +25,
wikipedia +73 / −48, github +119 / +19, youtube +137 / +93, openstreetmap −66 / −117 (the YouTube renderer goes),
pdf +110 / +64, the YouTube watch page +47 / −15, the video file −110 / −8.

**What these sessions did not do.**

- **Sites fail in the second hour** (p16: the first 31 site steps pass, then more and more fail; p8 the first 28;
  p4 fails the PDF from round 1). Cause: `[fork] arena alloc FAILED (128 MiB, pool has 137 MiB)`, 64 to 116
  times per session. The **process pool** is empty, not the demand pool: every round leaves one more idle
  utility process behind (`video_capture.CrUtility`, started by the video steps; also `unzip`, `data_decoder`),
  each with a 128 MiB arena, and after 27 processes (13 on 4 GiB) no renderer can be forked. The fork is refused
  with ENOMEM and gives back what it took; nobody is ended. **BUG-156, open.** See section 4 for sessions
  without the video steps.
- **No video played.** YouTube: the consent dialog "Before you continue to YouTube" was answered in round 1 of
  each session with a real click on "Reject all" (wheel to its buttons, click, dialog gone:
  `yt-consent-before.png`, `yt-consent-buttons.png`); after that the player shows, literally, **"Sign in to
  confirm you're not a bot. This helps protect our community. [Sign in] [Learn more]"** (`yt-bot-wall.png`).
  Nothing was done about it (no login, no proxy). The public video file (Big Buck Bunny, 360p VP9, Wikimedia
  Commons) opens in the browser's player with its length (10:34) and the pause symbol, and stays at 0:00 with a
  black picture and no sound (`video-file-stalled.png`): the media pipeline does not advance on EuroOS. Not a
  memory matter and not looked into here. The memory curve therefore contains the watch page and the player
  with a loaded file, not playback.
- **Reclaim was not needed.** The lowest free was 416 MiB (4 GiB guest): `pcache_reclaim` and the out-of-memory
  path never ran in a browser session. They are shown by `[memt] oom` (KVM and TCG) and by the host tests only;
  a 3 584 MiB guest (q35, 33 min) has a larger demand pool (1 437 MiB) than the 4 GiB one and did not get there
  either.

## 4. Two-hour sessions with the site matrix only (no video steps)

`chrome-mem-soak.py --no-video --free-video-seconds 0`, same image (`after-s8-curve.txt`, `after-s16-curve.txt`,
`session-s4.txt`, `s4-nmi.txt`).

| Guest | Result |
|---|---|
| 8 GiB (s8) | 25 rounds in 124 min. The first 52 site steps pass (8 rounds and 4 sites, ~55 min), then steps fail: 27 processes, `[fork] arena alloc FAILED` 106 times (BUG-156 without any video step). Demand pool: in use 930 → 921 MiB (802-1 225), −49 MiB/h, lowest free 2 007 MiB, no kill, unattributed 0 at the end (38 MiB at most) |
| 16 GiB (s16) | 9 rounds in 127 min. The first 16 site steps pass; from round 3 the tab stays on YouTube with an empty address and every navigation times out (12 processes, no refused fork, no abort: what BUG-135 describes). Demand pool: in use 878 → 694 MiB, lowest free 9 284 MiB, no kill |
| 4 GiB (s4) | **the guest hung after 5 minutes** (4 sites passed): "the desktop loop did not answer the serial console within 40 s". The NMI probe found the BSP in `sched::schedule_core+0x27c` with interrupts off, 153 live tasks, 572 MiB of the demand pool and 387 MiB of the process pool free, no `[oom]`, no refused fork. **BUG-157, open, cause not found.** It happened once in 12 browser sessions on this image; the task-slot reuse of BUG-151 changes what the scheduler sees and is not ruled out |

So the exit criterion "the site matrix repeated for two hours without a kill and with a flat curve at 4, 8 and
16 GiB" stands like this: **no browser was ended for memory and the demand pool is flat (falling) at all three
sizes; the site matrix itself does not hold for two hours at any size** (BUG-156 at 8 GiB after ~55 min and at
16 GiB with the video steps after ~60 min; a navigation stall at 16 GiB without them; the PDF step at 4 GiB,
where the 13th process does not fit the process pool; one hang at 4 GiB).

## 5. Regression round

| Check | Result |
|---|---|
| `cargo test` on the host (`cargo-test.txt`) | 187 test binaries, 1 360 tests passed, 0 failed (base: 1 347; new: `euromm` pagecache 4, ramfile 5, oom 3, vaspace 1) |
| TCG matrix of 10, self-test image of `1dd0d58`, against `../chrome-2026-11b/tcg-*.markers` (`comparison-tcg.txt`, `tcg-summary.txt`, `tcg-*.markers`) | 10 of 10 to the desktop; **0 worse in all 10**; new: `[memt]` OK (5 modes at 1 GiB; "not run on this machine" at 512 MiB) |
| the same matrix one commit earlier, image of `f9a8870` (`tcg-first-matrix/`) | **not 0 worse**: `[memt]` FAILED in the four 1 GiB configurations (its forks did not fit the 160 MiB process pool with a 192 MiB arena; fixed in `1dd0d58`, the test only); `[3g2-wire] WATCHDOG TRIPPED` in two boots (the flake of the base, BUG-085; not in the second matrix); `[mr-crash]` FAILED once in `combined-smp1-iommu-1g` (OK in the second matrix; cause not looked into) |
| KVM boots of the self-test image at 1, 2, 4, 8 GiB (`kvm-selftest-boots.txt`) | `[memt]` OK in all four; FAILED lines of the whole boot: 0 at 4 and 8 GiB, 1 at 2 GiB and 6 at 1 GiB (forks and a 200 MiB malloc of other self-tests on a small pool, as in the base) |
| R630 soak 2 h 05, 16 GiB, 4 vCPU, `restrict=on`, self-test image of `f9a8870` (`soak-summary.txt`) | completed clean (loadtest.py), 3 575 commands; 460 `[churn-proc]` OK lines; httpd 846 387 answered, 0 failed, p50 5.4 ms, p99 155 ms; **kernel heap +0.30 kB/min after 10 min, range 1 264 kB** (base: +0.24 kB/min, 1 262 kB); no panic, GP, page fault or NMI. Markers of the run against `../chrome-2026-11b/r630-soak-boot.markers`: **0 worse**, new `[memt]` (`soak-compare.txt`) |

Not run: the CI q35 matrix, `gui-smoke.py` on the plain public image, the 100 short boots, boot time.

## 6. Not done, and what is open

- BUG-156 (idle utility processes never end) and BUG-157 (one hang at 4 GiB): open, see above.
- The pool split: the process pool takes up to half of the RAM for 128 MiB arenas; at 4 GiB it holds 12
  processes and the PDF step needs a 13th. Measured, not changed.
- Page-cache eviction and the out-of-memory path never ran in a real browser session (nothing came near the
  limit). The path "out of memory inside a system call" has no test at all. Dirty pages and writeback exist in
  `euromm::PageCache` and its host tests; the kernel has no dirty page-cache pages and never calls that half.
- Named RAM files (profile, caches) are still byte vectors in the kernel heap: 7-53 MiB in these sessions; at
  4 GiB the kernel heap grew 120 → 164 MiB over two hours (+12 MiB/h), not attributed.
- `sysinfo` and `/proc/meminfo` tell programs the machine has 256 MiB; Chromium sizes its caches from that.
- The four commits `48bf2e7` to `f9a8870` were made from the tested tree by replaying the patch scripts; the
  intermediate trees were built and booted only at three points (after BUG-150/152, after BUG-153/155, the end).

## 7. Deviations from the rules

- The four baseline sessions (after 1 h 45 to 2 h 05) and two obsolete test sessions were stopped by hand, each harness and QEMU by its own PID.
- Screenshots were copied to the laptop to look at them (consent dialog, bot wall, video player); no source.
- `explore-consent.py` and `pilltest.py` (throw-away scripts on the R630 to look at the consent dialog) are not
  in the repository.
- One `pgrep -f` was used on the R630 to wait for one of my own scripts to end (nothing was killed with it).
- The TCG boots on .16 ran with the unrestricted user-mode NIC of `tcg-boot.sh`, as the base they are compared with.

## Files

`before-*-curve.txt`, `before-*.csv`, `after-*-curve.txt`, `after-*.csv` (mem-curve.py) · `session-*.txt` (one
line per step of every session) · `memt-before.txt`, `memt-after.txt`, `kvm-selftest-boots.txt` · `tcg-*`,
`comparison-tcg.txt`, `tcg-first-matrix/` · `soak-summary.txt`, `r630-soak-boot.markers`, `soak-compare.txt` ·
`cargo-test.txt` · `yt-consent-before.png`, `yt-consent-buttons.png`, `yt-bot-wall.png`, `video-file-stalled.png`
· `s4-nmi.txt`. Serial logs stay on the R630 (`/srv/euroos/mem-runs/`).
