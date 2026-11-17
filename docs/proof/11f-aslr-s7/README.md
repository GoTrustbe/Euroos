# ASLR step S7: merge with the BUG-380 fix, check, documentation (11f-aslr-s7)

Branch `feature/11f-aslr-s7` (worktree `/opt/euroos/wt-11f-aslr7` on .16), from `feature/11f-aslr-s6` `55c6e25`; `feature/11f-bug380`
(`19f7586`) merged in `4599d0e`. Not pushed. R630 run directory `/srv/euroos/aslr7-runs`. No kernel code was changed in S7.

## The merge

Only conflict: `docs/LOADTEST-TRACKER.md` (both sides kept: the S6 line of BUG-088 and the BUG-380 / BUG-371 line;
BUG-380 and BUG-371 fixed, BUG-390 and BUG-400 open). The automatically merged places of `kernel/src/ring3.rs`
(`vdso_tick`, `VdsoWriter`, `vdso_interp`, `vdso_rdtsc`, `vdso_clock`, the test accessors), `kernel/src/forkvdsotest.rs` and
`kernel/src/shell.rs` (`uptime` prints the vDSO clock) were read: the BUG-380 side touches only the clock page code; the S6
side only the arena segment code; they share no function and no line. The build needed no change.

## Method

Images built with `drivers/mkimg.sh` (as `scripts/build.sh` without the userland step: userland binaries and their signatures
copied from the main checkout, nothing rebuilt or re-signed, kernel signed for the image only, one cargo target directory per
source tree, `-j 6`, `nice -n 10`). Three self-test images: merge (`4599d0e`), S6 (`55c6e25`, from `git archive`), BUG-380
(`19f7586`, from `git archive`); one public image of the merge (and a copy with `ui.tour=off`, `update.check=off` for Chromium).
Images were streamed .16 -> laptop -> R630 and not stored on the laptop.

## Results

| check | result | file |
|---|---|---|
| cargo test (default-members) | 188 suites, **1396 passed, 0 failed**, 1 ignored (same as S6) | `cargo-test-summary.txt` |
| KVM self-test boots 1/2/4 GiB, 2 vCPU, quiet host, merge vs `55c6e25` | 3/3 to the desktop, 0 crash lines, **0 worse**; 1 GiB: `[vdso-mono]` FAILED (S6) -> OK (merge) | `kvm-markers-compare.txt` |
| same, merge vs `19f7586` | 3/3, 0 crash lines, **0 worse**; `[aslr-ind]` FAILED -> OK (S6 in the merge), `[arena-segs]` new and OK | `kvm-markers-compare.txt` |
| merge markers, 1/2/4 GiB | `[vdso-mono]`, `[aslr]`, `[aslr-ind]`, `[aslr-exec]`, `[aslr-phys]`, `[arena-segs]` OK in 3 of 3 (varying bits exe 30, ld.so 30, mmap 30, stack 29, brk 29, vDSO 27, as in S6) | `kvm-markers-compare.txt` |
| TCG matrix, 10 legs (smep+smap, restrict=on) | **10/10 desktop**, 0 watchdog, 0 NMI, 0 crash lines; **`[vdso-mono]` OK in every leg** (S6: FAILED in 8 of 10); `[aslr]` and `[aslr-ind]` never FAILED | `tcg-matrix.txt` |
| gui-smoke, public image, KVM 4 vCPU 4 GiB | **67/67** | `gui-smoke.txt` |
| Chromium, 5 starts | **5/5 PASS**, 0 aborts, 0 ring-0 faults, 0 FROZEN, 0 `[oom]`, 48 exec lines | `chrome.txt` |
| Chromium, one sites round | **14/14 steps**, 0 aborts, 0 ring-0, 23 exec lines; YouTube "reject all" clicked, the other four sites showed no banner | `chrome.txt` |

Not run (this is the merge check only; a full validation by another team follows): `aslr=off` and `legacy`, the 8 GiB boots,
q35 metal matrix, installs, soaks, 20 starts, interaction.

## Documentation (this step)

| file | change |
|---|---|
| `docs/design/ASLR.md` | status header (S0 to S6 done, S7 done, S8 open); section 12 "As built": zones, segments, the draw, entropy per region and per distance (numbers of the S01, S5, S6 proofs), boot options, limits, S8, corrections; a note in section 8 |
| `docs/BOOT-OPTIONS.md` | every value and default of `aslr`, `aslr.arena`, `aslr.split` checked against `kernel/src/bootopt.rs` and `ring3.rs` (`ASLR_DEFAULT` 1 = on, `ARENA_MODE_DEFAULT` 2 = random, `ARENA_SPLIT` true, parse functions, `arena_fixed`, `arena_split_name`): all correct; `legacy` "will be removed" softened to "may be removed"; link to SECURITY-ASLR.md |
| `docs/SECURITY-ASLR.md` (new) | what it does, what you notice, the order of the ways back, known limits |
| `docs/UPDATES.md` | release notes for `NEXT` (no version, no date): ASLR, BUG-353 (writable vDSO, a security flaw), BUG-350 to 355, 371, 380, open points BUG-390, BUG-400 (both "open, under investigation"), BUG-370, BUG-372, BUG-341 |
| `docs/LOADTEST-TRACKER.md` | merge conflict resolved; S7 line under BUG-088; statuses of BUG-088, 350, 351, 352, 353, 354, 355, 371, 380 (fixed) and 370, 372, 390, 400 (open) checked against the proofs: they agree |

## Contradictions and findings (nothing guessed)

- Design section 8 said an exec'd image has no vDSO; since the clock fix (BUG-355) it has (27 varying bits in every exec'd
  child, `11f-aslr-s5/chrome-entropy.txt`). Noted in section 8 and 12.6.
- Design section 4 foresees a shell command `aslr`; there is none in `kernel/src/shell.rs`.
- BUG-341 is named in the task and in `feature/validate-2026-11f` (`548336b`, in `feature/app-control`) but not in this branch's
  tracker (the branch does not contain that commit). Release notes name it; the tracker entry was not copied (it would conflict
  on the later merge).
- Website: the site sources are not tracked in this repository (`web-staging/` is git-ignored in the main checkout, English
  only: no FR/NL sources there). A search for ASLR, randomisation, address space and memory protection found no claim about
  ASLR; the only memory claim ("Memory-safe end to end", Rust `forbid(unsafe_code)` libraries) is not affected. No text was
  prepared, nothing deployed, `/var/www` not touched.

## Deviations

- CPU range: gui-smoke (8-11) and the 5 Chromium starts (12,32) ran before the coordinator limited the runs to 0-7 and 20-27;
  both passed and were not repeated. The first sites run (13,33) was stopped by PID on that message (`sites-aborted-cpu-move.out`
  is not kept) and run again on 3,23. KVM boots ran on 0-2/20-22, the TCG matrix on 4-6/24-26.
- One image of each of S6 and BUG-380 was built from `git archive` trees with the ignored userland binaries copied from the
  main checkout, as the earlier steps did.
- Drivers used: `drivers/` (`mkimg.sh`, `kboot.sh`, `tcgboot.sh`, `tcgq.sh`, `stage-kvm.sh`, `stage-tcg.sh`, `stage-ui.sh`).
