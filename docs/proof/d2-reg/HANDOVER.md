# D2-reg handover (2026-11-15)

Branch `feature/d2-reg` from `feature/d2` `2cf4320`, worktree `/opt/euroos/wt-reg` on .16, not pushed. R630 run directory
`/srv/euroos/reg-runs` (CPUs 0-39). Build helper on .16: `/tmp/reg/build.sh TAG COMMIT|- [st pub br]` ->
`/tmp/reg/img/TAG-KIND.img`; images go to the R630 through the laptop (gzip stream; .16 has no key for the R630).
Experiment worktree `/opt/euroos/wt-reg-x` (detached, scratch; remove at the end). `toolchain/eupkg/keys/dev.key` is a
symlink in both worktrees (remove at the end).

## Commits
| Commit | What |
|---|---|
| `8a6f333` / `c70dddf` | BUG-250 test (`[root-heap]`) / fix (RAM root moved into the cache, not cloned) |
| `0cf75e6` / `72976f7` | BUG-251 serial sender (no IF=0 polling, port outside the lock, every print deferred) + low-end flag / flag removed again |
| `f42be2a` | BUG-252 driver budget (nice -15 within 10 % of every 500 ms, nice 19 past it, debt forgiven per window) |

## Final images: tag `r2` (= `72976f7`), `img/r2-{st,pub,br}.img` on the R630
Final regression: `phaseR.sh r2` (KVM boots, TCG matrix, gui-smoke, soak 2 h, Chromium smp 2 sites x3 / interact /
localvideo on tmpfs work directories); collect with `collect-r.sh` into `out/final/`; keylat in `out/keylat-final.txt`.

## Measurement note
Chromium keylat runs must use tmpfs work directories (`cms.sh`): on the root disk, with soaks beside them, QEMU's
screendump writes (6 MB each, under the device lock) stall the vCPUs and every keylat number doubles (d1 823-1083 ms there,
330-455 ms on tmpfs).
