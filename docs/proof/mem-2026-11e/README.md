# Copy-on-write fork, named RAM files in frames, sysinfo (2026-11-15)

Branch `feature/mem-2026-11e` from `feature/app-control` `3ecde9e`. Not merged, not pushed.
Design: `../../design/MEMORY-RECLAIM.md` section 3 and section 8. Bugs BUG-240 to BUG-243 in `../../LOADTEST-TRACKER.md`.

| Commit | What |
|---|---|
| `c793def` | the test first: `[memt] cowfork` (`gmem2`, new and signed), the model of `euromm::cow` (5 host tests) |
| `dc0b337` | copy-on-write fork: arena and demand pages shared, holder counts per frame, write faults from ring 3 and ring 0 |
| `d29ff3d` | 4 KiB arena pages only for demand-paged processes (`[robust-paging]`, `[bug053]` FAILED with them for every process) |
| `685af41` | the tests first: `[memt] ramnamed`, `[memt] sysinfo` (FAIL on that tree) |
| `4c0467d` | sysinfo and /proc/meminfo report the machine (BUG-240) |
| `92f619b` | named RAM files are frames of the demand pool (BUG-241, BUG-242) |
| `aca0cd2` | design, section 8 |
| `9c30378` | the holder counts in the kernel heap (BUG-243: in frames they moved the boot layout and `[bgnet-cap]` failed) |

Images: self-test `st-f3` (`9c30378`); public `pubo-f2` (`d29ff3d`, `ui.tour=off;update.check=off`) for the browser
runs, `plain-f2` (`d29ff3d`, no options) for gui-smoke. `9c30378` only moves the count table from frames into the
heap; the browser runs were not repeated on it. R630, KVM `-cpu host`, CPUs 12-23; packs copied from
`/srv/euroos/chrome-runs/`. Serial logs stay in `/srv/euroos/mem3-runs/`.

## 1. Copy-on-write fork

`[memt] cowfork`: a process of 64 MiB (8 MiB arena, 56 MiB demand pages) forks; KVM, 4 GiB (`memt-before-after.txt`).

| | before (`c793def`) | after |
|---|---|---|
| frames the fork takes | 72 336 kB | 340 kB (1 GiB: 352 kB) |
| fork, wall time in the parent (irq windows included) | 301 ms | 123 ms |
| kernel `[fork]` line, arena / demand pages | 111 / 124 ms | 118 / 0 ms the first fork of the process, 0 / 0 ms later forks |
| checks | 14/17, FAILED | 17/17 at 1, 2, 4, 8 GiB and in the four 1 GiB TCG configurations |

The checks: parent, child and grandchild see the memory of the fork and never what another writes later, also for a
`read()` of the kernel into a shared page (child and parent), MADV_DONTNEED in the child, munmap in the parent; the
child gets frames only for what it writes; every frame back at the end.

Browser, 2 hours at 4 GiB, all `[fork]` lines (`chrome-soak2h.txt`, `fork-times.txt`):

| | chrome-mem2 p4 (before) | this session |
|---|---|---|
| forks | 130 | 132 |
| arena per fork | 23 632 KiB copied, 88 ms mean | 0 KiB copied (23-24 MiB shared), 7 ms mean (21 ms the first, 0-1 ms later) |
| demand pages per fork | 115 ms mean | 99 ms mean |
| arena + demand, median / p90 / max | 192 / 331 / 627 ms | 96 / 191 / 291 ms |

The demand part did not get much faster: what is left there is mapping the page-cache pages (read-only, as before),
copying the windows of MAP_SHARED files (as before) and the irq windows; the private pages are no longer copied.

## 2. Named RAM files

`[memt] ramnamed` (16 MiB to `/tmp`, read back after close, stat, mmap, rename, O_APPEND, ftruncate, O_TRUNC, ENOSPC,
unlink): before 9/15 (4 GiB) and 8/15 (1 GiB; the heap grew 16 384 kB), after 15/15 at 1, 2, 4, 8 GiB.

Two-hour session at 4 GiB (`chrome-soak2h.txt`, `soak2h-curve.csv`): **kernel heap 117 → 118 MiB (min 117, max 119,
+0.6 MiB/h)**; before (chrome-mem2 p4, same harness) 117 → 143 MiB. `ramfile-bytes` (files in the heap) 1 MiB
throughout; the named files are in the `shm` frames.

## 3. sysinfo and /proc/meminfo

`[memt] sysinfo`: before totalram 131 072 kB and freeram 0 (wrong offsets, BUG-240), MemTotal 262 144 kB; after
(4 GiB) totalram 3 427 616 kB, freeram 2 454 336 kB, sharedram 8 192 kB, MemTotal = totalram; 7/7 at 1, 2, 4, 8 GiB.

## 4. The two-hour session (`chrome-mem-soak.py --local-video-dir`, 4 GiB, `chrome-soak2h.txt`)

11 rounds in 7 462 s, **178 of 179 steps ok**, local VP9+Opus clip with sound 11/11 (12 of 12 windows, RMS 1158).
0 FROZEN, 0 aborts, 0 refused forks, 0 `[oom]`, 0 isolation, 0 NMI, 0 fault lines. Demand pool in use 745 → 742 MiB
(657-946), +23.6 MiB/h (+1.8% of the pool per hour; chrome-mem2 p4: +16.0), lowest free 347 of 1 293 MiB.

- The failed step: `openstreetmap` in round 5: the page loaded (after 1.6 s, load finished 6.0 s later) but its content
  was "still changing" at the deadline (`screenshots/soak2h-osm-fail.png`); the same step passed in the other 10
  rounds. The R630 ran my TCG matrix and TCG reruns next to the session then. Not looked into further.
- mem-curve prints "NOT flat" because of ONE census with 77 MiB without an owner (line 120 455 of the serial log): it
  ran while the browser was in a fork (the `[fork]` line of pid 1121 follows it), so pages already shared with a child
  that was not on the list yet were counted half. The same remark as chrome-mem2 p4 (109 MiB). All other censuses: at
  most 3 MiB.

## 5. Regression round

| Check | Result |
|---|---|
| `cargo test` (`cargo-test.txt`) | 187 binaries, **1 368 passed, 0 failed** (base 1 363; new: `euromm::cow` 5) |
| KVM self-test boots 1, 2, 4, 8 GiB, HDA (`kvm-selftest-boots.txt`) | desktop in all four, 0 crash lines; FAILED lines 2/0/0/0 (at 1 GiB `GBIG malloc(200 MiB)`, as in validate-2026-11d); `[memt]` 10 modes OK at all four |
| markers-compare against validate-2026-11d, KVM (`comparison-kvm.txt`) | **0 worse** at 1, 2, 4, 8 GiB |
| TCG matrix of 10, restrict=on (`tcg-summary.txt`) | 10/10 desktops, 0 watchdog, 0 NMI, 0 fault lines, 0 FROZEN; `[memt]` 10 modes OK in the four 1 GiB configurations |
| markers-compare against validate-2026-11d, TCG (`comparison-tcg.txt`) | **0 worse in 10/10** (on `d29ff3d`: `[bgnet-cap]` worse in the four 1 GiB configurations, BUG-243, fixed by `9c30378`) |
| `gui-smoke.py`, plain public image, KVM 4 GiB (`gui-smoke-public.txt`) | **67/67** |
| Chromium site matrix, 3 rounds (`chrome-sites3.txt`) | **36/36** |
| Chromium interaction (`chrome-interact.txt`) | **18/18** |

## 6. Not done, open

- The browser runs (site matrix, interaction, the session, gui-smoke) ran on `d29ff3d`; `9c30378` (count table in the
  heap instead of frames) was checked with the KVM boots and the TCG matrix only.
- BUG-243: why `/bin/msock` does not run with its arena at 0xc200000 is not found (worked around).
- The arena of the launched process stays identity mapped (virt = phys): its frames are pinned in the copy-on-write
  model and its base cannot be randomised (the ASLR question of the kbugs team). Its 4 KiB page tables are a first step
  toward a virtual arena; the kernel's accesses to it by address are what is left.
- Copy-on-write rests on ring 3 running on one core (no TLB shootdown) and on the kernel writing into a process only
  through that process's page tables; neither is checked by a test.
- The demand part of a fork (page-cache mappings, MAP_SHARED windows) still costs ~100 ms in the browser.

## 7. Deviations

- `userland/glibc/gmem2` and `gmem2.sig` are committed (a new test binary and its signature, signed with the dev key as
  gmemt was); no other `.sig` and no `dev.pub` changed.
- Other teams' guests ran on the R630 at the same time (also on 16-23 at first); at most 4 of my guests at a time.
- `4c0467d` and `92f619b` were split from one tested tree (the sysinfo commit was not built alone).
- Images and proof text went between the servers through the laptop as a stream, not stored.
