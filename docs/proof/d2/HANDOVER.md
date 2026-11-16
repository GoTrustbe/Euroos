# D2 handover (2026-11-15)

Branch `feature/d2`, worktree `/opt/euroos/wt-d2` on .16, not pushed. R630 run directory `/srv/euroos/d2-runs`.
Build helper on .16: `/tmp/d2/build.sh TAG [st pub br]` -> `/tmp/d2/img/TAG-KIND.img` (`br` = public image with
`ui.tour=off;update.check=off`, the browser image of `chrome-smoke.py`; a `chrome` profile image is NOT that).
Patch scripts and test sources of this round: `/tmp/d2/wip/`. `toolchain/eupkg/keys/dev.key` is a symlink in the
worktree (remove it at the end).

## State
| Step | Commits | Result |
|---|---|---|
| A integration | `fc0b105` `3f75d74` `b6eb05d`, proof `fba3b8f` | cargo test 1 381/0, KVM 1-8 GiB, TCG 10/10, gui-smoke 67/67; BUG-250 (storage configs at 1 GiB) open |
| B BUG-231 | `eedd5bf` | drivers at nice -15: hammer p50 79.5 -> 4.7 ms; `[fair]` worst 1.5 s, `[wakelat]` 0.05 ms |
| S3 | `a64534e` (test first, FAILED 0/3), `fce6567` | `[ap-user]` OK smp 2/4/8, `[ap-parity]` OK, smp 1 ⬜ |
| S4 | `97e0fcf` `a899923` `776d17a` `f7f2f29` `9cbebe8` (tests), `12c83fe` | `[kill-smp]` OK (200 smp 8, 300 smp 8), `[many-threads]` 320/320 on APs, `[cow-smp]` OK; `[tlb-smp]` criterion fixed in `9cbebe8`, run on `b8` pending |
| S5 | `0018719` | `[spread]` 0.51-0.60 (want <= 0.40): FAILED, cause open; keylat A/B smp 4 off 628-648 / on 675-840 ms, smp 2 off 619-680 / on 673-774 ms: criterion NOT met, `smpuser` stays off by default |
| S6 | `80c028d` | `[ap-idle]` 0 timer interrupts in 10 s (before 1 000) |

## Found on the way
- keylat regression from the integration, not from S3-S6: same host, same harness, `--smp 4`: sched-d1 image 342 ms
  (kernel total 220 ms), integration `b6eb05d` 839 ms (kernel total 551 ms). The serial TX task (t4,
  `serial::drain_load`) runs 33 % instead of 9 % with IF=0 windows of 50-68 ms (`[if0-window] cause=kernel task 4`).
  Not bisected (candidate: one of small/kbugs/mem; logs `runs/pm3x-4`, `runs/pd1-4`). New bug BUG-251.
- `[cow-smp]` right after `smpuser noshoot` + `tlbsmp` + `smpuser shoot` ran its workers on the BSP (0/3 on their AP)
  in two runs (c4-n, c7-n); not analysed.

## Next
1. Final regression done (README.md, final/); open points below.
2. BUG-251 bisect (m3 images: `fc0b105`/`3f75d74` builds of `pub` + `br` conf) and BUG-250 heaptrace.
3. S5: why the APs are only 41-55 % busy in `[spread]` (sample `smpstat` during the run), bounce cost 0.3-0.5 ms.
