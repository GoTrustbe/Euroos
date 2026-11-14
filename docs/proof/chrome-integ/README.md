# Chromium on EuroOS, integration of w23, mem and harness (2026-11-14)

Branch `feature/chrome-integ` from `feature/chrome-fdspace` `40f853a`, worktree `/opt/euroos/wt-integ` on .16.
Not pushed. Bugs: `../../LOADTEST-TRACKER.md` (BUG-180 fixed, BUG-181 new; integration notes on BUG-140/141/147/150/151).

| Commit | What |
|---|---|
| `7d6d073` | merge `feature/chrome-w23` `4ba91f9` (BUG-140..147, BUG-160/170/171, harness `6ac5e85`): no conflicts |
| `3990f17` | merge `feature/chrome-mem` `5b9b6de` (BUG-150..155, census, `[memt]`): the task table resolved as one design (below) |
| `01bdda3` | merge `feature/chrome-harness` `b943448` (wait_loaded, scroll_settled, reject-all consent): tracker BUG-162 only |
| `85c84fd` | BUG-180, the test first: `[memt] forkkill`; `[memt]` runs get a 360 s deadline |
| `af6f415` | BUG-180: a task ended inside a fork gives back what the fork took |
| (last) | this proof, the tracker (BUG-180, BUG-181, integration notes) |

Images (all built on .16 with `scripts/build.sh`, -j 6): `f-*` = `af6f415` (final), `i1-*` = `01bdda3` (the merges alone),
`t1-before` = `85c84fd`. `-st` self-test, `-pub` public with `ui.tour=off;update.check=off`, `-plain` public without options.
Baselines for the comparisons, same R630 and options: the memory team's `final2-st` (`bddf3a5`) and the w23 team's
`m1-selftest` (`95a3bf2`), booted in full at 1, 2, 4 and 8 GiB (the memory team's own KVM logs stop at `[memt]`).
R630, CPUs 0-19 only, run directory `/srv/euroos/integ3-runs` (serial logs, wavs, screenshots stay there).

## 1. The task table: two fixes of the same bug, one design

Both teams found that the task slots of ended fork children never came back (w23: BUG-140, mem: BUG-151), that a fork
refused late kept the child's arena (BUG-141 = BUG-150), and that 256 slots and 224 thread kernel stacks were too few
for a browser session (BUG-147 = the second half of BUG-151). They changed the same lines:

| | w23 | mem | integrated |
|---|---|---|---|
| retire a task | `retire_task`: futex entries, kernel stack, network waiter, GLIBC_THREADS/GLIBC_CTIDS/THREAD_NAMES, then RECLAIMABLE (if Dead) | `retire_task`: mark_dead + RECLAIMABLE | one function: mark_dead, then everything of w23 (and since BUG-180 a fork in flight); idempotent; every task retired once (the auto-merge retired each twice) |
| reuse of a slot | `alloc_slot` clears the RECLAIMABLE mark of the new occupant, resets caps/profile | (not) | w23's: without it the sweep could hand a slot out twice |
| order in exit_group of a worker | slot offered before fdtab::exit/proc_state_drop of the child main | same | main marked Dead at once, retired after its descriptors and state (kept by task number) are gone |
| refused fork | `fork_undo()` | `undo_fork` closure + `[spawn-refused]` reason + `[memt]` hook | mem's (same frees, plus the log and the reason) |
| task table | 512 | 512 | 512; fd spaces (`MAX_SPACES`) 512 |
| thread kernel stacks | 224 static + heap up to 480 (`kstack_base`) | 224 static + heap up to 448 (`thread_kstack_base`) | 224 static + heap up to **480** (32 of the 512 slots stay for kernel tasks and background processes); one allocator; on a full heap the dead-owner sweep still runs (w23) instead of giving up at once (mem) |
| census | `slot_census` (fds) | `live_tasks` (memstat) | both; `live_tasks` = `slot_census().1` |

The tests of both teams stay and pass: `[churn-forkthreads]`, `[many-threads]`, `[syncsock]`, `[fdsem]`, `[memt]`.

## 2. Results (final image `af6f415` unless said otherwise)

| Check | Result |
|---|---|
| `cargo test` on the host (`cargo-test.txt`) | 187 test binaries, **1 363 passed, 0 failed**, 1 ignored (w23 1 350 + mem's 13 new) |
| KVM self-test boots 1, 2, 4, 8 GiB (`kvm-selftest-boots.txt`) | desktop in all four, 0 crash lines. `[memt]` 6 modes OK at all four; `[many-threads]` 320/320 at all four; `[churn-forkthreads]` 40/40 and `[syncsock]` 17/17 at 4 and 8 GiB, SKIP at 1 and 2 GiB ("fork refused for memory", as in the w23 boots of the same sizes); `[fdsem]` OK |
| markers-compare against both teams (`comparison-kvm.txt`) | against w23 `95a3bf2`: **0 worse** at 1, 2, 4, 8 GiB. Against mem `bddf3a5`: **0 worse** at 4 and 8 GiB; at 1 and 2 GiB `fork` FAILx8 → FAILx10 and FAILx7 → FAILx9: the arena allocations of `[churn-forkthreads]` and `[syncsock]` (w23 tests, not in the mem tree) that a 1-2 GiB pool refuses; the same two lines are in the w23 boots, where the comparison is 0 worse |
| TCG matrix of 10 (`tcg-summary-f.txt`) | **10/10 to the desktop, 0 watchdog trips, 0 NMI, 0 fault lines**, longest desktop gap 1.4-1.8 s; `[memt]` 6 modes OK in the four 1 GiB configurations ("not run" at 512 MiB). The first matrix (`01bdda3`, `tcg-summary-i1.txt`): also 10/10 and 0 trips, `[memt]` FAILED once: BUG-180, section 4 |
| TCG markers (`comparison-tcg-*.txt`) | against the w23 team's TCG markers (`7b0629c`, same R630 conditions): **0 worse in 6**; in the four 1 GiB configurations `fork` FAILx9 → FAILx10, the arena of the newer `[syncsock]` that 1 GiB refuses (its verdict "not run", OK), as in the w23 team's own comparison. Against the memory team's (`.16`, NIC without `restrict=on`) and `chrome-2026-11b`: `udrv-net`, `udrv-crash`, `udrv-linux` and `conn` FAILED ("no DNS reply", TCP handshake to an outside address) because the R630 boots run with `restrict=on`, the same difference the w23 team recorded; `fork` +2 as in the KVM boots; nothing else worse |
| site matrix, 3 rounds in one session (`site-matrix-fs1.txt`) | **PASS 36/36**, 0 aborts, 0 refused forks, 0 `[oom]`, peak 197 live tasks of 512, 0 slots held by ended tasks, `unattributed=0 KiB` (`01bdda3`: 36/36, peak 200) |
| interaction matrix (`interact-fx1.txt`) | **18/18 PASS** (also 18/18 on `01bdda3`) |
| local video (`localvideo-fv1.txt`) | VP9+Opus: **currentTime 2.56 → 60.97 s, 28/28 screendumps differ, 12/12 five-second windows of the wav above the noise floor (RMS 1150-1158)**, no media error; Opus alone 2.9 → 60.9 s, 12/12; VP9 alone plays; H.264/AAC fails as before (BUG-161, the build has no such codecs) |
| Wikimedia video of the memory team (`wikimedia-video.txt`) | see section 3 |
| `gui-smoke.py`, plain public image, `restrict=on`, 4 GiB (`gui-smoke-public.txt`) | **PASS 67/67** (also 67/67 on `01bdda3`) |
| BUG-157 (hang in schedule_core) | not seen: no NMI probe, no "did not answer", no watchdog line in any of the boots of this work (19 KVM boots, 22 TCG boots, 11 browser and GUI sessions) |

## 3. The Wikimedia video that stayed at 0:00

The memory team's file (`.../Big_Buck_Bunny_4K.webm.360p.vp9.webm`) now answers **404** from Wikimedia ("File not found",
`screenshots/wm-360p-file-not-found.png`); it cannot be replayed. The 480p transcode of the same video (VP9 + **6-channel**
Opus) in the browser's own player, two times 90 s per boot, on `01bdda3` and on `af6f415`: **the picture moves (22 of 22
screendumps differ) and the time runs**; the tab shows its speaker (`screenshots/wm-480p-plays-silent.png`). So the stall at
0:00 is gone with the merge; that it was BUG-160 fits (an audio track stalled every clip at t=0 before BUG-170/171) but
cannot be proven on a file that is gone.

**But there is no sound** (0 of 18 windows, the whole wav silent). Serial: `ALSA lib pcm.c:8822:(snd_pcm_set_params)
Channels count (6) not available for PLAYBACK`, `PcmSetParams: hw:0,0, Invalid argument`; `[alsa] open`, four refines,
`close`, no `hw_params`. The kernel starts Chromium with `--alsa-output-device=hw:0,0`, our PCM offers 2 channels, and with
an explicit device Chromium does not downmix. Stereo clips play with sound (section 2). **BUG-181, open** (next: accept 1-8
channels in `alsa.rs` and downmix into the stereo ring; test first with a 5.1 clip).

## 4. BUG-180, found by the TCG matrix

The first matrix (`01bdda3`, `tcg-summary-i1.txt`) had 10/10 desktops and 0 watchdog trips, but in `storage-smp1-iommu-1g`
`[memt] forkmany` was killed by the 120 s deadline of its run in its 263rd fork (418 ms per 96 MiB arena copy under TCG on
the R630; 370 ms in two reruns of the same image, which passed, `bug180.txt`) and the process pool lost 96 MiB:
`process pool 16384 (was 40960) → FAILED`. A forking task that is ended inside the fork (it yields between the pieces of
the arena copy) never finishes or undoes it. Test first (`85c84fd`, `[memt] forkkill`, KVM): before 427008 → 402428 frames
(4 GiB) and 40960 → 16381 (1 GiB), FAILED; after (`af6f415`): `[fork] task 25 ended inside a fork: ... 96 MiB of arena and
tables given back`, 0 lost, OK. `[memt]` runs now have a 360 s deadline (forkmany needs ~120 s under TCG on the R630).

## 5. Not done, open

- Two-hour sessions, BUG-156 (idle utility processes, 128 MiB arena each) and BUG-157: for the Fable team, as agreed.
- BUG-181 (5.1 audio silent): recorded, not fixed.
- BUG-161 (H.264/AAC: build), BUG-145/146 (typed URL, key to screen): unchanged.

## Deviations

- Two kernel commits on the integration branch besides the merges (BUG-180, test first), because the TCG matrix found it.
- The Wikimedia check used the 480p transcode (the 360p one is gone) and a driver outside the tree
  (`/srv/euroos/integ3-runs/drivers/wmvideo.py`, which calls `video_play` of `scripts/chrome-mem-soak.py` unchanged).
- KVM baselines were booted fresh from the teams' images (`bddf3a5`, `95a3bf2`); the memory team's `1dd0d58` image was
  not on the R630 (`1dd0d58` changes only the `[memt]` arena size; the integrated tree has it).
- `userland/{doom,muslreal,muslfile}.elf` copied from the main checkout (no musl-gcc on .16), as the other worktrees have.
