# ASLR step S6 (docs/design/ASLR.md): image, heap and stack segments at bases of their own

Branch `feature/11f-aslr-s6`, worktree `/opt/euroos/wt-11f-aslr6` on .16, from `feature/11f-aslr-s5` `7b2eea6`, not pushed.
R630 run directory `/srv/euroos/aslr6-runs`. Images built with `drivers/mkimg.sh` (`scripts/build.sh` without the userland step:
userland binaries and their signatures copied from the main checkout, none rebuilt or re-signed; one cargo target directory
per source tree). on = default (`aslr=on`, `aslr.arena=random`, `aslr.split=on`), off = `aslr=off`, legacy = `aslr=legacy`;
base = `7b2eea6` (S5) with the same option. CPUs: 0-39 at first, from the coordinator's change on only 0-15 and 20-35 for
every new run (runs already pinned to 16-19 and 36-39 ran to their end; their queued follow-ups were stopped and run again
on the allowed CPUs).

| commit | what |
|---|---|
| `fcb4736` | tests first: `euromm::layout` segments and their host tests (4 fail), `paging` arena functions with a segment list (one segment everywhere), self-test `[arena-segs]`, `[robust-paging]` case `arena_segs`, `[aslr-ind]` judges every segment pair |
| `831894f` | S6: the draw (`DRAW_BYTES` 40), `split_segs`, arena zone 8..20 TiB, the kernel on segment lists, boot option `aslr.split=on|off` |
| (docs) | design section 11, BOOT-OPTIONS (`aslr.split`, `aslr.arena`), tracker BUG-088, this directory |

What changed, the cut and what the design got wrong: `docs/design/ASLR.md` section 11. The audit: `audit.md`.

## Tests first (`tests-first.txt`)

| test | `fcb4736` (tests, no fix) | `831894f` |
|---|---|---|
| host `split_segments_in_their_windows`, `split_entropy`, `split_region_distances`, `split_off_and_legacy` | FAILED (no segments: `split_segs` None) | ok (61 of 61 euromm tests) |
| `[arena-segs]` (three segments through fork, copy-on-write, exec move, teardown) | FAILED: no lazy arena at 13 and 16.5 TiB (zone ended at 12 TiB) | OK in every boot |
| `[robust-paging]` case `arena_segs` | FAILED: refused with 8 of 8 frames | OK (31 refusals, 7 builds) |
| `[aslr-ind]` | FAILED: stack-mmap 10, exe-stack 10, exe-mmap 15, stack-ld.so 10, exe-brk 14, brk-stack 10 | OK: 31, 32, 31, 32, 31, 31 |

Corrected after the tests-first commit (neither touches a >= 20-bit criterion): the `[robust-paging]` totals (23/6) had to
count the new case (31/7); a host-test bound for the pairs inside one segment (`<= 11` varying bits) was wrong for the
difference of two page slides (14 bits counted) and became a range check of the distance.

## Entropy (`entropy.txt`, `chrome-entropy.txt`)

21 KVM boots per image (2 GiB, 2 vCPU), `[aslr]` = 64 starts of the embedded probe `gaslrx` per boot; "over boots" = the
first start of every boot. Chromium = 20 starts per image, the launched browser and every exec'd child.

| region | before `7b2eea6`: over boots / per boot | after `831894f`: over boots / per boot |
|---|---|---|
| embedded program (PIE) | 30 / 30 | 30 / 30 |
| ld.so | 30 / 30 | 30 / 30 |
| stack | 29 / 29 | 29 / 29 |
| brk heap | 29 / 29 | 29 / 29 |
| small mmap (4 KiB) | 30 / 30 | 30 / 30 |
| mmap 1 MiB, libc | 30 / 30 | 30 / 30 |
| vDSO | 27 / 27 | 27 / 27 |
| Chromium arena (image) base, heap segment, stack segment (launch and exec) | 21, -, - | 21, 21, 21 |

| distance | before: over boots / `[aslr-ind]` per boot | after: over boots / per boot |
|---|---|---|
| exe - heap (brk) | 14 / 14 | **31 / 31** |
| heap - stack (brk - stack) | 10 / 10 | **31 / 31** |
| exe - stack | 10 / 10 | **32 / 32** |
| exe - small mmap | 15 / 15 | **31 / 31** |
| stack - small mmap | 10 / 10 | **31 / 31** |
| stack - ld.so | 10 / 10 | **32 / 32** |
| exe - mmap 1 MiB (per boot: libc - exe) | 34 / 34 | 34 / 34 |
| exe - vDSO | 30 / (not printed before S6) | 31 / 31 |
| same segment: ld.so - exe, small mmap - brk | 12, 14 | 12, 14 (page slides only, by design) |
| Chromium ld.so - brk, brk - stack, ld.so - stack (launch / exec) | 12, 10, 10 / 12, 10, 10 | **31, 31, 32 / 31, 31, 32** |
| Chromium exe - brk, exe - stack, exe - floor, exe - vDSO (launch) | 32, 32, 32, 30 | 31, 33, 32, 31 |

`[aslr-ind]` FAILED in 21 of 21 boots before, **OK in 21 of 21** after; `[aslr]` OK 21/21 on both. None of 185 exec'd
Chromium children kept an address of the launched browser in any region (exe, ld.so, arena, heap and stack segment, brk,
stack, floor, vDSO). `aslr=off`: 0 bits everywhere (image 8 TiB, heap 12 TiB, stack 16 TiB, 20 starts and 190 execs);
`aslr=legacy`: the layout of before S1, unchanged.

## Audit (`audit.md`)

Every place that turns an arena address into another one, with the S4 rule that a place doing silently less is a fault.
Found by reading and fixed in `831894f`: the zone end of 12 TiB under every walk that frees or counts arena tables (a heap or
stack segment would have leaked its tables and been left out of the census), the copy-on-write page scan of
`[fork-cow-write]` (it walked `base .. base + span` and saw only the image segment), and the non-lazy wipe in
`do_child_execve` (one `arena_fill` over the whole span would have written into the gap between segments: a ring-0 fault).
The reaper's copy-on-write sweep goes by RAM, which is still one piece: correct unchanged. No new path reaches user memory
from another address space or hands it to a device (A10/A11 of S4).

## Results (`831894f`)

| check | on | off | legacy | file |
|---|---|---|---|---|
| cargo test (default-members) | 188 suites, **1396 passed, 0 failed** (S5: 1390; +6 host tests) | (same code) | (same code) | `cargo-test-summary.txt` |
| KVM self-test 1/2/4/8 GiB, 2 vCPU, HD-Audio, vs base same option | 4/4 to the desktop, 0 crash, **0 worse**; `[aslr-ind]` FAILED -> OK, new `[arena-segs]` OK | 4/4, 0 crash; 2 GiB: `[vdso-mono]` OK -> FAILED while the TCG matrix ran (BUG-380), quiet rerun 3 of 3 OK (base 3 of 3), else 0 worse | 4/4, 0 crash, **0 worse** | `kvm-markers-compare.txt` |
| other placements, KVM 2 GiB, vs base same option | `aslr.arena=fixed` 0 worse, `identity` 0 worse, `aslr.split=off` vs base default 0 worse (`[aslr-ind]` FAILED there, as with one base) | | | `kvm-markers-compare.txt` |
| entropy boots, KVM 2 GiB, 21 per image | `[aslr]` OK 21/21, `[aslr-ind]` OK 21/21, 0 crash (base: `[aslr-ind]` FAILED 21/21) | | | `entropy.txt` |
| TCG matrix (10 legs, smep+smap, restrict=on), vs base | **10/10 desktop**, 0 watchdog, 0 NMI, 0 crash; vs base: `[aslr-ind]` FAILED -> OK in every leg; `[vdso-mono]` FAILED in 8 of 10 legs, base 8 of 10 (BUG-380: 2 legs each way) | | | `tcg-matrix.txt` |
| IOMMU legs (6) | fault-record lines per leg equal to the base (the deliberate faults of the malicious-driver self-tests) | | | `tcg-matrix.txt` |
| q35 metal matrix, KVM, legs one after the other | **15/15 PASS**; tpm/tpmcrb not run (no swtpm) | | | `q35-matrix.txt` |
| gui-smoke, public image, 4 vCPU 4 GiB | **67/67** | **67/67** | **67/67** | `gui-smoke-installs.txt` |
| 60 short TCG boots to `[fuzz]` | **60/60**, 0 FAILED, 0 crash | | | `short-boots.txt` |
| installs: wizard; ro-install plain/inject/ro0 | 15/15; 6/6, 3/3, 5/5 | | | `gui-smoke-installs.txt` |
| Chromium 20 starts, smp 2 | **20/20**, 185 execs, 0 aborts, 0 ring-0, 0 FROZEN, 0 `[oom]` | **20/20**, 190 execs, 0/0/0/0 | **20/20**, 188 execs, 0/0/0/0 | `chrome.txt` |
| sites, 3 rounds (YouTube "reject all" clicked) | **36/36** (base beside it 36/36) | 1 round: **14/14** | 1 round: **14/14** | `chrome.txt` |
| interaction | **18/18** | | | `chrome.txt` |
| local VP9+Opus with sound (one clip a round in the soaks) | picture 28/28, sound 12/12 windows in every round (5 of 5, twice) | | | `chrome.txt` |
| chrome-mem-soak 1 h, 8 GiB | **123/123 PASS** (soak2-on), base beside it 123/123; first soak: 123 of 123 open-codec steps ok, see deviations; 0 aborts, **0 ring-0 faults**, 0 FROZEN, 0 `[oom]`, 0 "mmap window full" | | | `chrome.txt` |
| loadtest soak 1 h, 16 GiB, 4 vCPU | 5 864 commands, 0 errors, **0 watchdog, 0 panic**, `[churn-proc]` 0 FAILED of 4 324, `[oom]` 3 (the `[memt]` self-test) | | | `loadtest-soak.txt` |

Ring-0 faults, watchdogs and panics in every S6 run above: **0**. The smaller small-mmap window (the heap segment ends
4 MiB before the arena end) gave no "arena mmap window full" line in any Chromium run, and no process was ended by a fault
(a stack past its segment would have been).

## Deviations and open points

- **CPU range.** Midway the coordinator gave CPUs 16-19 and 36-39 to the BUG-380 team. The runs already pinned there (two
  entropy lanes, one TCG lane of the base) ran to their end; their queued follow-ups were stopped by PID (`tcgq.sh` of the
  base lane on 36-39; the entropy lanes by their stop file) and the missing work ran on 0-15/20-35.
- **Entropy boots ran twice.** The first 44 boots used the repository's `short-boots.sh`, which deletes a clean log and keeps
  no `[aslr]` line: they count as health boots only (s6: 22 of 22 without a FAILED line). The numbers above are from 21 + 21
  boots with `drivers/ent-boots.sh` (the same script, also keeping the `[aslr]` lines).
- **Clip directory.** The first pair of soaks served a clip directory that also held `h264.mp4`; the soak tried it once a
  round, the browser refused it (no H.264 decoder), 25 steps failed. Not an open-codec test; both soaks ran again with
  VP9 + Opus only (`vid-open`): 123/123 each.
- **BUG-390 (new), on the base.** The first base soak (`7b2eea6`) stopped answering in round 1: the browser main thread spun
  with interrupts off in `child_mem_swap` (NMI probes in `chrome.txt`). Seen once; the rerun of the same base soak and every
  S6 run passed. Filed, not fixed (out of scope); S6 has the same code path.
- **`[vdso-mono]` (BUG-380).** TCG: 8 of 10 legs for S6 and for the base; KVM: once (`aslr=off` 2 GiB while the TCG matrix
  ran), 3 of 3 OK on a quiet host for S6 and base. Not caused by S6; the clock code was not touched (BUG-380 has its own team).
- **Corrections after the tests-first commit**: the `[robust-paging]` totals and one host-test bound (see above).
- tpm/tpmcrb legs of the q35 matrix not run: no swtpm on the R630. `chrome-smoke --only localvideo` not run (it needs
  `h264.mp4`): VP9+Opus with sound is measured in the soaks, as in S4 and S5.
- TCG matrix, q35, short boots, installs, interaction and the soaks ran with `aslr=on` only; `off` and `legacy` got cargo test
  (same code), the KVM boots at 1/2/4/8 GiB, gui-smoke, 20 Chromium starts and one sites round each.
- The kernel images are signed for the image by `drivers/mkimg.sh` (a signature inside the image, as `scripts/build.sh`
  does); no `.sig` file of the tree was made or changed.

## New bugs

BUG-390 (the browser main thread spins with interrupts off in `child_mem_swap`; seen once, on the base). BUG-391..399 not used.

## What S7 (docs) needs

- Boot options: `aslr.split` (done in `docs/BOOT-OPTIONS.md` with this step) and the changed text of `aslr.arena=random`
  (three parts between 8 and 20 TiB).
- Release notes: the program's own memory is cut into three parts at places of their own; the distance between them is
  random (31..32 bits; before 10..15); `aslr.split=off` is the way back; the stack has a 4 MiB part (at least 2 MiB of room)
  and a stack that grows past it ends the program.
- Design section 11 (numbers above), tracker BUG-088 (S6 line), BUG-390.
- Website EN/FR/NL only if ASLR is mentioned there (not checked in this step).
