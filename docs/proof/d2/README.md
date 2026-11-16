# D2: integration, BUG-231, SMP scheduler steps S3 to S6 (2026-11-15)

Branch `feature/d2` from `feature/sched-d1` `be87a0c`, worktree `/opt/euroos/wt-d2` on .16, not pushed. R630 run directory
`/srv/euroos/d2-runs` (all 40 CPUs). Final code commit `9cbebe8` (image tag `b8`); `smpuser` stays **off by default** (the S5
criterion is not met). Step A: `integration.md`; step B: `bug231/README.md`; final regression: `final/`; step tests: `final/step-tests*.txt`.

| Commit | Step |
|---|---|
| `fc0b105` `3f75d74` `b6eb05d` `fba3b8f` | A: merges small, kbugs, mem (one textual conflict, tracker); proof; BUG-250 |
| `eedd5bf` | B: BUG-231, userspace drivers at nice -15 |
| `a64534e` / `fce6567` | S3 test first (`[ap-user]` FAILED 0/3) / S3 |
| `97e0fcf` `a899923` `776d17a` `f7f2f29` `9cbebe8` | S4-S6 tests first, then test corrections (demand-paged memory, `smpuser noshoot` knob, deadlines, stale-read criterion) |
| `12c83fe` / `0018719` / `80c028d` | S4 / S5 / S6 |

## Criteria per step (KVM, R630)

| Step | Criterion | Before | After |
|---|---|---|---|
| B | hammer p50 under a busy desktop | 79.5 ms (sched-d1) | **4.7 ms** (S0 5.6) |
| S3 | `[ap-user]` smp 2/4/8 KVM and TCG; `[ap-parity]`; smp 1 identical | 0/3 on an AP (`a64534e`) | **OK smp 2/4/8 KVM, TCG 1 GiB smp 4**; parity OK; smp 1 ⬜. TCG 512 MiB: gapuser not launched (exit -1 in 7 ms, arena memory), marker FAILED there |
| S4 | `[tlb-smp]` x1000, `[kill-smp]` x1000, `[many-threads]` 320/320 on APs | stale reads in dontneed/cowbreak (`b3`; `noshoot` on `b8`: 97/100, 94/100) | **0 stale in 3 x 1000**, `[kill-smp]` **2 x 500 OK** (census 11 -> 11; 1000 in one run exceeds the 15 min run deadline, smp 8 300 OK), `[many-threads]` 320/320 with 4 604 AP switch-ins, `[cow-smp]` OK; shootdowns reported in `smpstat` |
| S5 | `[spread]` <= 0.4; keylat smp 4 median < 100 ms, smp 2 not worse | ratio 1.00 (no placement) | ratio **0.40-0.60** (failed by rounding on `b8`, 0.51-0.60 earlier); keylat (`final/keylat-ab.txt`, `chrome-perf.txt`) smp 4 off 495/664 ms, on 1011/595 ms; smp 2 off 619-680, on 673-774 ms: **not met, default stays off** |
| S6 | `[ap-idle]` < 5 timer interrupts in 10 s | 1 000 per AP | **0** |

Where the keylat time goes: not in the scheduler. Same host, same harness, `--smp 4`, smpuser off: sched-d1 image 342 ms
(kernel `total` 220 ms), integration `b6eb05d` 839 ms (kernel total 551 ms). The serial TX task (`serial::drain_load`) takes
33 % of the CPU instead of 9 %, with IF=0 windows of 50-68 ms: **BUG-251**. With the APs the bounce round trip of a system
call is 0.3-0.5 ms (two IPIs, two switches, halted vCPUs before S6); chrome's threads bounce 300-500 times a second each,
so `smpuser=on` gains nothing yet; S7 (futex on the AP) is the next lever.

## Final regression of `9cbebe8` (`final/`)
| Test | Result |
|---|---|
| cargo test | 188 binaries, 1 381 passed, 0 failed, 1 ignored |
| KVM self-test boots 1/2/4/8 GiB | 4/4 desktops, 0 crash lines, 0 inversions; FAILED lines `[aslr]` (expected) + GBIG at 1 GiB; markers-compare vs validate-2026-11d **0 worse** |
| TCG matrix 10 | **10/10 desktops, 0 watchdog, 0 NMI, 0 inversions**; 0 worse in 6, 3 worse in the 4 storage/combined 1 GiB configurations (BUG-250, from step A) |
| q35 (KVM, restrict) | base, nvme, ahci, e1000e, hda, usb, usbhub, usbaudio, usbnet, hwprobe, scan, power PASS; nvmeroot/ahciroot FAIL as on earlier images; the script stops at `tpm` (no `swtpm`), `printer` not run |
| gui-smoke public | **67/67** |
| Chromium smp 4 | sites 3 rounds **36/36**, interaction **18/18**, local VP9+Opus picture and sound 12/12 (H.264/AAC fails by design, 16/21) |
| soak 2 h | 6 019 commands ok, 0 errors, 0 panic/NMI/FROZEN/inversions, churn proc median 2.9 s **worst 5.3 s** (limit 12.6), hammer **p50 4.6 ms, 3 702 001 requests** (3ecde9e: 5.4 ms, 827 487; sched-d1: 60.9 ms, 440 262). **Worse:** console commands per hour 5 630 -> 2 887, churn fs 8.5 -> 18.2 s, one `WATCHDOG TRIPPED` during a 20 s `churn fs` (BUG-252) |

## Not done / open
- S5 criterion (keylat < 100 ms) not met; `smpuser` default off; S6's "default on" therefore not switched.
- `[spread]` <= 0.40 not reliably met (APs 41-55 % busy in the 8-thread run; cause not found).
- BUG-250 (1 GiB storage configs), BUG-251 (serial TX / keylat regression of the integration), BUG-252 (console commands
  slower under the hammer, one watchdog trip) open; `[cow-smp]` after `smpuser noshoot` toggling ran its workers on the BSP twice.
- `lockstress` addition of the design not written (the AP paths take no lock).
- The browser image (`br`) is the public image with `ui.tour=off;update.check=off`; an early run used the `chrome` profile by mistake (`p5-*`, `pm3-*`: invalid, they hit a `cdp_send` panic of that image, not analysed).
