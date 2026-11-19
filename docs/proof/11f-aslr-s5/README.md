# ASLR step S5 (docs/design/ASLR.md): the arena base drawn per launch and per exec

Branch `feature/11f-aslr-s5`, worktree `/opt/euroos/wt-11f-aslr5` on .16, not pushed. Started from `feature/11f-aslr-s4`
`ce6ca19` with a running merge of `feature/11f-bugs-35x` (`9c4d420`). R630 run directory `/srv/euroos/aslr5-runs`, CPUs 0-39
pinned per lane. Images built with `drivers/mkimg.sh` (= `scripts/build.sh` without `userland/build.sh`; userland binaries and
signatures from the main checkout, no signature changed). on = default (`aslr=on`, `aslr.arena=random`), off = `aslr=off`,
legacy = `aslr=legacy`; base = `417f6b9` (the merge: S4 layout) with the same option.

| commit | what |
|---|---|
| `417f6b9` | merge of `feature/11f-bugs-35x` (BUG-351/352/355); the only conflict, `docs/LOADTEST-TRACKER.md`, kept both sides |
| `35d7bab` | tests first: host tests `on_arena_in_its_zone`, `on_arena_entropy`, `arena_off_and_legacy`; self-test `[arena-move]` with a refusing stub |
| `e9b488d` | S5: arena base (21 bits) and page slides of exe and ld.so (9 bits each) per launch and per exec; exec moves the child's arena; `aslr.arena=random|fixed|identity`, `random` the default |
| (docs) | design section 10, BOOT-OPTIONS, tracker (BUG-088 closed, BUG-380), this directory |

What changed and what the design got wrong: `docs/design/ASLR.md` section 10.

## The merge first (`merge-check.txt`)

Before S5: KVM 2 GiB boot of `417f6b9` against `ce6ca19` and against `9c4d420`: **0 worse**; only new: `[fork-cow-write]`,
`[fork-fault-wait]`, `[vdso-mono]` OK (against `9c4d420`: `[aslr-phys]` FAILED -> OK, the S4 change). cargo test 188 suites,
1387 passed, 0 failed. One Chromium start: PASS 5/5. Read after the automatic merge of `ring3.rs`/`aslrtest.rs`:
`demand_file_backed` (BUG-351) returns early for every address below the demand zone, so arena addresses (8..12 TiB) never
reach the per-page rule; the vDSO of an exec'd child (BUG-355) is mapped at the new draw in PML4 slot 200, apart from the
arena zone (slots 16..23) whose chain the exec frees and rebuilds; `test_read_as_parent` and `[fork-cow-write]` use the
virtual arena of the parent. Nothing to change.

## Tests first (`tests-first.txt`)

| test | `35d7bab` (tests, no fix) | `e9b488d` |
|---|---|---|
| host `on_arena_entropy` | FAILED (arena base 0 bits, want 21) | ok (55 of 55 euromm tests) |
| `[arena-move]` | FAILED: lazy_arena_rebase refused the move | OK in every boot |
| `[aslr]` | FAILED: PIE 0, ld.so 0, stack 8 | OK: PIE 30, ld.so 30, stack 29, mmap 30 |
| `[aslr-exec]` | FAILED: ld.so kept 8/8 | OK: 0/8 in every region |

One host test was corrected before the fix: it expected exactly 29 varying bits for the stack; the statistic can count 30
there (the subtraction of the slide borrows into the base bits), so it asks >= 29. The drawn bits (21 + 8) are unchanged.

## Entropy (`entropy.txt`, `chrome-entropy.txt`)

20 KVM boots per image (2 GiB, 2 vCPU), `[aslr]` = 64 starts of the embedded probe `gaslrx` per boot; Chromium = 20 starts
per image, the launched browser and every exec'd child.

| region | before `417f6b9`: over 20 boots / per boot | after `e9b488d`: over 20 boots / per boot |
|---|---|---|
| embedded program (PIE) | 0 / 0 | **30 / 30** |
| ld.so | 0 / 0 | **30 / 30** |
| stack | 8 / 8 | **29 / 29** |
| brk heap | 8 / 8 | **29 / 29** |
| small mmap (4 KiB) | 9 / 9..10 | **30 / 30** |
| mmap 1 MiB, libc | 30 / 30 | 30 / 30 |
| vDSO | 27 / 27 | 27 / 27 |
| Chromium exe (launch / exec) | 30 / 30 | 30 / 30 |
| Chromium ld.so (launch / exec) | 0 / 0 | **30 / 30** |
| Chromium brk, stack (launch / exec) | 8, 8 / 8, 9 | **29, 29 / 29, 29** |
| Chromium arena base (launch / exec) | fixed (8 TiB) | **21 / 21** |
| Chromium mmap floor, vDSO | 30, 27 | 30, 27 |

`[aslr]` OK in 20 of 20 boots (before FAILED 20 of 20). 199 exec'd Chromium children: none kept an address of the browser in
any region. `aslr=off`: 0 bits in every region (20 starts, 190 execs); `aslr=legacy`: exe 0, ld.so 0, brk 8, stack 8/9,
floor 20, vDSO 0 (as before S1).

## Results (`e9b488d`)

Stage 1 ran every lane at once on the R630 (chrome, loadtest soak, KVM boots, TCG matrix, entropy boots, short boots); the
reruns marked "quiet" ran with at most four guests on the host.

| check | on | off | legacy | file |
|---|---|---|---|---|
| cargo test (default-members) | 188 suites, **1390 passed, 0 failed** (merge: 1387; +3 host tests) | (same code) | (same code) | `cargo-test-summary.txt` |
| KVM self-test 1/2/4/8 GiB, 2 vCPU, HD-Audio, vs base same option | 4/4 to the end, 0 crash; **0 worse**; `[aslr]` and `[aslr-exec]` FAILED -> OK, new `[arena-move]` OK | 4/4, 0 crash, **0 worse**, new `[arena-move]` OK | 4/4, 0 crash; 1 GiB: `[vdso-mono]` OK -> FAILED under host load (BUG-380), rerun quiet 3 of 3 OK (base 3 of 3), 0 worse; other sizes 0 worse | `kvm-markers-compare.txt` |
| entropy boots, KVM 2 GiB, 20 per image | `[aslr]` OK 20/20, `[aslr-exec]` OK 20/20, `[arena-move]` OK 20/20, 0 crash | | | `entropy.txt` |
| TCG matrix (10 legs, smep+smap, restrict=on), vs base | **10/10 desktop**, 0 watchdog, 0 NMI, 0 crash; **0 worse** in every leg; `[vdso-mono]` FAILED in 6 legs (base: 9), BUG-380 | | | `tcg-matrix.txt` |
| IOMMU legs (6) | only the deliberate fault records of the malicious-driver self-tests, the same counts as the base | | | `tcg-matrix.txt` |
| q35 metal matrix, KVM, legs one after the other | **15/15 PASS**; tpm/tpmcrb not run (no swtpm) | | | `q35-matrix.txt` |
| gui-smoke, public image, 4 vCPU 4 GiB | **67/67** | **67/67** | **67/67** | `gui-smoke-installs.txt` |
| 60 short TCG boots to `[fuzz]` | **60/60**, 0 FAILED, 0 crash | | | `short-boots.txt` |
| installs: wizard; ro-install plain/inject/ro0 | 15/15; 6/6, 3/3, 5/5 | | | `gui-smoke-installs.txt` |
| Chromium 20 starts, smp 2 | **20/20**, 199 execs, 0 aborts, 0 ring-0, 0 FROZEN, 0 `[oom]` | **20/20**, 190 execs, 0/0/0/0 | **20/20**, 186 execs, 0/0/0/0 | `chrome.txt` |
| sites, 3 rounds (YouTube "reject all" clicked) | loaded host: 34/36 (youtube, openstreetmap in round 3: slowest console answer 2804/1859 ms); quiet A/B: **36/36**, base 36/36 | | | `chrome.txt` |
| interaction | **18/18** | | | `chrome.txt` |
| local VP9+Opus with sound (in the soaks, one clip a round) | picture 28/28, sound 12/12 windows in every round (5 of 5, twice) | | | `chrome.txt` |
| chrome-mem-soak 1 h, 8 GiB (sites + interaction + local clip per round) | loaded: 5 rounds, 122/123 steps (openstreetmap: map area still blank); quieter: 122/123 (the same step); A/B pair: **123/123 PASS**, base 123/123 PASS; every soak 0 aborts, **0 ring-0 faults**, 0 FROZEN, 0 `[oom]` | | | `chrome.txt` |
| loadtest soak 1 h, 16 GiB, 4 vCPU | 5 770 commands, 0 errors, **0 watchdog, 0 panic**, `[churn-proc]` 0 FAILED of 4 255, `[oom]` 3 (the `[memt]` self-test) | | | `loadtest-soak.txt` |

Ring-0 faults, watchdogs and panics in every run above: **0**.

## Deviations and open points

- **Sites and soak steps.** With every lane running, sites lost two steps in round 3 (YouTube and openstreetmap: the desktop
  loop answered the console after 2.8 s and 1.9 s; the limit is 1 s, as in the S4 deviation). The A/B rerun on a quieter host
  gave 36/36 for S5 and for the base. In two of three S5 soaks one step failed: openstreetmap with the map area still blank
  after the load (`soak-on-q-openstreetmap-fail.png`, page and controls drawn, no tiles yet); the third S5 soak and the base
  soak next to it passed 123/123. No child crashed, aborted or was ended in any of them (no `ended by signal`, no `[abort]`),
  so the cause is not visible from EuroOS; tile loading from the internet (four browsers on one datacenter address) is the
  likely one. Not proven either way; not filed as a bug.
- **`[vdso-mono]` (BUG-380).** Fails under TCG (base 9 of 10 legs, S5 6 of 10) and once in a loaded KVM boot (legacy 1 GiB).
  Pre-existing (BUG-355 code of `9c4d420`), not caused by S5; filed, not fixed (out of scope).
- **Build tooling.** One `CARGO_TARGET_DIR` for several source trees made cargo take a build of one tree for another (git
  archive gives the files their commit time, older than the output): the first base image of `9c4d420`, the first S5 dev image
  and the first cargo test of the merge were stale. Found from the boot logs (missing self-tests, `aslr.arena=fixed`), every one
  rebuilt with a target directory per tree; the numbers above are from the rebuilt images only.
- `chrome-smoke --only localvideo` needs `h264.mp4`, which is not used (open codecs only): VP9+Opus with sound is measured in
  the soaks, as in S4.
- tpm/tpmcrb legs of the q35 matrix not run: no swtpm on the R630.
- TCG matrix, q35, short boots, installs and the soaks ran with `aslr=on` only; `off` and `legacy` got cargo test (same code),
  the KVM boots at 1/2/4/8 GiB, gui-smoke and 20 Chromium starts each.
- `aslr.arena` keeps the value `fixed` (S4 layout) next to `identity`; the design foresaw only `on/off/legacy` (section 10).

## New bugs

BUG-380 (the vDSO clock steps back by microseconds under TCG and on a loaded host). BUG-381..389 not used.

## What S6 needs

`docs/design/ASLR.md` section 10, last paragraph: three segments (image, heap, stack) over the same RAM with their own draws,
the arena functions and `ChildMem` with a segment list, `[aslr-ind]` as the test.
