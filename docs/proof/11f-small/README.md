# Sprint 11f, team C: small items, tests, docs

Base `e4b8ba4`, branch `feature/11f-small`. Numbers are in the files listed; test counts are not marketing, they are what was run.

| Item | Result | Proof |
|---|---|---|
| BUG-282 q35 legs `nvmeroot`, `ahciroot` | Harness, not the kernel: a 512 MiB guest has 59 MiB of heap free for the 131 MiB install media (installer unavailable), and the install wants a target above 512 MiB. Scripts now 1 GiB guest, 1 GiB target. Both legs PASS (phase A and B). | `bug282-q35-root-legs.txt`, `q35-matrix.txt` |
| keylat / chrome-smoke omnibox | The perf key step pressed Ctrl+L and typed after 0.3 s without looking; `navigate` gave up silently after 3 unseen Ctrl+L. New `focus_omnibox`: Ctrl+L up to 3 times, each judged by the selection on the screen, then Esc + one click, else the screendump is kept and the step fails. Not reproduced (the flake is rare); `--only perf` on the R630 saw the selection every time (6 loads). | commit `3a52bc6` |
| Reproducible image | FAT volume id, label time and file/directory times came from the clock: 32 bytes differed between two builds. `mkfs.fat -i`, `SOURCE_DATE_EPOCH` (default: commit time), ESP laid out from a staging tree, `mcopy -m`. Two builds: identical sha256; files in the ESP identical to the old image. | `repro-image.txt` |
| BUG-275 | Reproduced in 100 TCG boots: 4 of 97 desktops "(no result line)". Cause: the 120 s program deadline, killed at fork 37-40 of 41 while progressing. Test budget 6 min. Same 40 boots after: 40/40. | `bug275-tcg100.txt`, `bug275-tcgfix40.txt`, `bug275-no-result-line-4-logs.tar.gz` |
| BUG-320 (new, found by the 100 boots) | 3 of 100 boots wedged: X client spins on `XCONNS` (IF=0) while task 0 was preempted inside `pump_mouse` holding it. `[xlock]` FAILED -> OK; `pump_mouse` and `window_at` lock with interrupts off. | `bug320-*`, `bug275-tcgfix40.txt` |
| BUG-280 | The deadman counted a loop that waits on a slow disk, request after request, as hung (`churn fs` of 36 s). virtio-blk completions of the pet task now count as life. `[wd-io]` FAILED -> OK. The slow host disk itself was not reproduced. | `bug280-watchdog-io.txt` |
| Docs | New `docs/BOOT-OPTIONS.md` (smpuser, sched.drvbudget/drvwindow, other options, open codecs); `docs/UPDATES.md`: date-based version numbers and what changed in 2026.11.16. There is no Evaluator's Guide in this repository (tree and history), so nothing was changed there. | commits |

## Final checks

- `cargo test` (default members): `cargo-test-summary.txt`.
- q35 matrix on the final image, KVM, restrict=on: 15 legs PASS (all but `tpm`, `tpmcrb`: no swtpm): `q35-matrix.txt`.
- One KVM boot, markers-compare against the self-test image of `e4b8ba4`: 0 worse, 2 new markers OK (`[wd-io]`, `[xlock]`): `kvm-markers-compare.txt`.

## Notes

- The R630 runs used CPUs 18, 19, 38, 39 only, work directory `/srv/euroos/11fc-runs` (removed at the end).
- The 100-boot baseline ran while my KVM runs shared CPUs 18/38: three of the four BUG-275 failures are on 18, one on 38, none on 19/39. The load is part of the explanation, not an excuse: the deadline was too tight for a loaded host.
