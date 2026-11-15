# Validation of `feature/d2-reg` (release candidate, 2026-11-15)

Release candidate `feature/d2-reg` `2657918` (code `72976f7`), merged with the newer `feature/app-control` commit `2593b6b`
(`.gitignore` only) into `feature/validate-2026-11e` `48dfd05`, worktree `/opt/euroos/wt-val` on .16. Not pushed. Images of `48dfd05`
(built with `scripts/build.sh`, `-j 6`, `nice 10`): self-test `4b5a4943...`, public `8a31e9f3...`, browser (public with
`ui.tour=off;update.check=off`) `beed7825...`; the release image of this build stays on the R630 as `/srv/euroos/val-runs/img/v1-pub.img`.
Baselines: validate-2026-11d (`../validate-2026-11d/`), the public image 2026.11.14 (download, `euroos-x86_64-uefi.img.gz`, raw sha256
`9b584e9d...`, booted here as `pubrel.img`) and, for the browser numbers, the release code `3ecde9e` built as a browser image (`b0-br`,
`ac0a721d...`). R630 run directory `/srv/euroos/val-runs` (CPUs 0-39), the scripts are in `drivers/`. Browser runs and keylat have their
work directories on tmpfs (`cms.sh`).

## 1. Verdict

**Klaar voor release: nee.** Two release criteria are missed and the D2 steps brought new regressions, all with a cause found or narrowed
down (tracker BUG-270 to BUG-275):

| Bug | Finding | Criterion | Introduced by |
|---|---|---|---|
| BUG-272 | boot time median **3.00 s** (download 2.68 s, 10 pairs, quiet machine); `smp-timer` phase 551 ms against 47 ms | median at most 2.8 s | S6 `80c028d` |
| BUG-271 | APs take no kernel work: `[smp] 0/3 AP(s) idle in hlt, taking kernel work` on the **public image** (download 3/3); the `[kwork]` self-test is gone | markers-compare "0 worse, explain every difference" | S6 `80c028d` (same cause as BUG-272) |
| BUG-270 | q35 matrix **12/13**: the `power` leg fails, the shutdown lines are lost | 13/13 | BUG-251 `0cf75e6` |
| BUG-274 | keylat on the browser **60 to 130 ms slower** than the release code `3ecde9e` in 16 of 16 pairs (e.g. 409 against 327 ms, smp 4, quiet) | compare with `3ecde9e` | not found (the kernel's own `[keylat]` numbers are better) |
| BUG-273 | 100 short boots: 21 of 108 end with `[fuzz] ... FAILED` (the fuzzer hits its 30 s limit on a loaded host); validate-2026-11d 0 of 88 | no unexplained FAILED | D2 family (also `9cbebe8`) |

Everything else is green: cargo test, KVM boots, TCG matrix (one intermittent failure, not reproduced), gui-smoke 67/67 in all variants,
wizard, 10 installs side by side, ro-install, rename-persist 24/24, the 2-hour soak (all four limits met), Chromium 20 starts, sites,
interaction, local video, Wikimedia 480p, and the 2-hour browser session at 8 GiB (179/179, heap flat). `smpuser=on` (not a criterion)
passes its smoke.

## 2. Results

| Test | Result | Comparison |
|---|---|---|
| a. `cargo test` (`cargo-test-summary.txt`) | 188 binaries, **1 381 passed, 0 failed**, 1 ignored | identical to d2-reg `72976f7` (11d: 187 binaries, 1 363) |
| b. KVM self-test boots 1/2/4/8 GiB, HD-Audio (`kvm-selftest-boots.txt`) | desktop in 4/4, 0 crash lines, 0 lock inversions; FAILED lines: `[aslr]` at all four (BUG-088 partial, known) and the two `GBIG malloc(200 MiB)` at 1 GiB | `comparison-kvm.txt`: **0 worse** against validate-2026-11d and against d2 `9cbebe8` at all four sizes. Differences: only in base `[kwork]` OK (now "no APs accepting work": **BUG-271**); new `[aslr]` FAILED (known), `[root-heap]`, `[keyfocus]`, `[ap-parity]`, `[ap-user]`, `[rename-disk]`, `[reseed]`, `[entropy-health]`, `[randid]`, `[nmi-bound]` OK |
| c1. TCG matrix of 10, restrict=on (`tcg-summary.txt`, `tcg-reruns.txt`) | **10/10 desktops, 0 watchdog, 0 NMI, 0 fault lines, 0 FROZEN, 0 inversions** | `comparison-tcg.txt` against validate-2026-11d: 0 worse in 9; **1 worse**: `combined-smp1-iommu-1g` `[churn-forkthreads]` "(no result line) (exit 0)" once; the same configuration again 3 times: PASS 3/3 (**BUG-275**, intermittent). Differences in all: `[kwork]` as above; `[ap-user]` FAILED in the three 512 MiB smp 4 configurations (gapuser is not launched with that arena memory, known from D2); the other FAILED lines (`udrv-net`, `conn`, `churn-proc`, `app-policy` in the 512 MiB ones, `GBIG`, `aslr`) are the same as the baseline (NIC `restrict=on`, small memory) |
| c2. q35 metal matrix, KVM, restrict=on, slirp bridge (`q35-matrix.txt`, `power-leg.txt`) | **12 of 13**: base, nvme, ahci, e1000e, hda, usb, usbhub, usbnet, usbaudio, scan, **printer**, hwprobe PASS; **`power` FAIL**. `tpm`/`tpmcrb` not run (no `swtpm` on the R630) | validate-2026-11d 12/13 (hwprobe), chrome-fix190 13/13. `power`: 3/3 FAIL on `48dfd05`, PASS 2/2 on 11d and on d2 `9cbebe8`; bisect `c70dddf` PASS 2/2, `0cf75e6` FAIL 2/2: **BUG-270** |
| d1. `gui-smoke.py`, public image, KVM 4 vCPU 4 GiB (`gui-smoke-pub.txt`) | **PASS 67/67** | download 2026.11.14 (`gui-smoke-rel.txt`) 67/67; `comparison-public.txt`: **0 worse** against 11d and against the download, only new `[root-heap]`, `[ap-parity]`, `[keyfocus]` OK. Difference: `[smp]` line 0/3 against 3/3 (**BUG-271**) |
| d2. `--soak 35` monkey, seed 20261114 (`gui-smoke-soak.txt`) | **PASS 78/78**, 2 003 actions | 11d: 78/78 |
| d3. `--input virtio`; `--net e1000e` (`gui-smoke-input.txt`, `gui-smoke-e1000e.txt`) | **PASS 67/67**; **PASS 67/67** | 11c/11d: PASS |
| e1. `wizard-install-test.py` | **15/15** | 15/15 |
| e2. ten installs side by side on eight CPUs (`installs-side-by-side.txt`, BUG-191) | **10 of 10 OK** (7 to 29 s) | download 2026.11.14: 10 of 10 (11d: the download failed 6 of 10) |
| e3. `ro-install-test.py` (BUG-220; modes plain, inject, ro0) | 14 checks **all PASS**: RAM-root fallback and desktop on a read-only target, `FAILED` verdict on the null disk, read-only disk not offered | new test |
| e4. boot time, public image, KVM, pin 38-39, alternating (`boot-time.txt`) | **median 3.00 s** (10 pairs, quiet: 2.70-3.39); the first block of 8 pairs under load 2.99 s | download **2.68 s** (2.23-2.93); 11d 2.54 s: **limit 2.8 s missed, BUG-272** |
| e5. 100 short boots of the self-test image (`short-boots.txt`, `fuzz-ab.txt`) | 108 boots, 0 crash lines, CSPRNG seeded, `[3d8]` `[3d7]` `[3d3]` `[linux-rand]` OK in all 108; **21 `[fuzz]` FAILED** (the only FAILED line) | 11d: 107 boots, 0 FAILED. A/B at the same time: 11d 0/88, d2 3/72, `48dfd05` 14/88; explained and tracked as **BUG-273** |
| e6. `rename-persist.py` | **24/24** | 11d 21/24 (BUG-111 fixed) |
| e7. loadtest soak 2 h, 16 GiB, 4 vCPU (`r630-soak-summary.txt`) | **10 668 commands, 5 119 an hour, 0 errors**; churn proc worst 3.3 s (limit 12.6), churn fs worst 11.5 s (limit 12), `[churn-proc]` 7 866 verdicts 0 FAILED; hammer 735 589 requests, 0 errors, **p50 5.5 ms** (limit 6); 0 watchdog, 0 panic/NMI/FROZEN, 0 inversions | d2-reg r2 5 217 an hour, fs worst 10.6 s, p50 5.6 ms; 11d 1 686 an hour, churn proc worst 43.9 s. The first attempt (a soak next to short boots on the SMT siblings of its CPUs) gave 3 340 an hour and churn fs worst 42 s: not valid, repeated with idle siblings (section 3) |
| f1. Chromium, 20 fresh starts (`chrome-starts20.txt`) | run 1 **19/20** (start 19: `[timer-probe] FROZEN`), run 2 **20/20** | `3ecde9e` 20/20 beside it; 11d 59/60. BUG-182 seen 3 times in about 55 browser boots, section 3 |
| f2. site matrix, 3 rounds, smp 2 (`chrome-site-matrix.txt`) | **PASS 36/36**, 0 aborts, 0 refused forks, 0 `[oom]`, 0 FROZEN | 36/36 |
| f3. interaction matrix (`chrome-interact.txt`) | **PASS 18/18** | 18/18 |
| f4. local video VP9+Opus with sound (`chrome-localvideo.txt`) | picture advances, **12 of 12** five-second windows with sound (RMS 1 158); H.264+AAC fails by design (16/21) | same as 11d and d2-reg |
| f5. Wikimedia 480p VP9 + 6-channel Opus, 2 x 90 s (`chrome-wikimedia-480p.txt`) | **PASS**: picture 22/22 screendumps differ in both rounds, sound in **16 and 17 of 18** windows | 11d 17 of 18 |
| f6. keylat, browser image, tmpfs, `3ecde9e` against `48dfd05` (`keylat.txt`) | quiet machine, medians: smp 4 410 / 409 / 428 ms, smp 2 (FROZEN) / 416 / 398 | `3ecde9e`: 327 / 307 / 327 and 289 / 278 / 322 ms: **60 to 130 ms slower in 16 of 16 pairs, BUG-274** |
| f7. ONE 2-hour session at 8 GiB, `chrome-mem-soak.py --local-video-dir` (`chrome-soak2h.txt`, `chrome-soak2h-curve.txt`) | **PASS 179/179**, 11 rounds in 7 373 s, 0 kills, 0 aborts, 0 refused forks, 0 `[oom]`, 0 FROZEN; heap flat (121 to 124 MiB used), in use +13.3 MiB/h (verdict "flat", 0.4 % of the pool an hour), demand pool free 2 466-2 477 MiB at the end of each round | 11d B: 179/179, +10.8 MiB/h |
| g. `smpuser=on`, smp 4 (informational; `gui-smoke-smp.txt`, `chrome-site-matrix.txt`) | boot + gui-smoke **67/67**; site matrix **36/36** | not a criterion |

## 3. Deviations and explanations

- **BUG-270** (`power-leg.txt`): `power::shutdown()` prints `[power] shutting down` and writes PM1a without `serial::emergency()`; since BUG-251
  (`0cf75e6`) the print sits in the deferred queue and the guest is off before the sender runs. QEMU exits within 1 s (direct run, `drivers/power-direct.sh`),
  but `[acpi] power button pressed` and `[power] shutting down` are not in the serial log. Bisect: `c70dddf` PASS, `0cf75e6` FAIL.
- **BUG-271 / BUG-272** (`ap-bisect.txt`, `boot-time.txt`): `sched::ap_park_demo_workers` waits up to 500 x 1 ms for the demo workers of the APs to
  park; since the tickless AP idle of S6 (`0018719` parks 1/1, `80c028d` 0/1, found with self-test images of four commits) they never do,
  so every boot loses 500 ms (`[boot-time] phase smp-timer: 551 ms` against 47 ms) and `kwork::start_accepting()` is never called: the per-CPU kernel
  work queues stay idle also on the public image. markers-compare reports this as "only in base: kwork OK", not as worse; the d2-reg and d2
  proofs carry the same line.
- **BUG-273** (`fuzz-ab.txt`): the fuzzer's deterministic 20 000-call sequence is cut at the run deadline of 3 000 ticks (`[glibc] TIMEOUT waiting for
  the process to exit`); on lanes whose SMT sibling runs another guest the D2 family (`9cbebe8`, `f42be2a`, `48dfd05`) fails about one boot in six, validate-2026-11d
  never. With only a busy loop on the siblings nothing fails (0/16 per image), on a quiet machine it passes (KVM boots). Not a kernel crash; a marker with too little margin
  or a slower fuzz path of the D2 family; cause not found. No other FAILED line in 108 boots.
- **BUG-274** (`keylat.txt`): the harness-visible key to screen is 60 to 130 ms longer than on `3ecde9e` in every pair (quiet machine and with load), but the
  kernel's `[keylat]` lines (ring to client, read to first draw, draw to screen) are shorter. The d2-reg proof compared with sched-d1, which already had the gap.
- **BUG-182** (`chrome-starts20.txt`, `starts19-frozen.txt`): 3 `[timer-probe] FROZEN` in about 55 browser boots of `48dfd05` (start 19 of 20, one keylat run with load, one on the quiet
  machine) and none in about 30 of `3ecde9e`; every time the first `connect` of the browser holds the CPU with interrupts off for 1.0 s (`[if0-window] 1009-1036 ms cause=syscall 42`).
  Known and open since the audio team (11d: 1 of 60); added to the tracker entry with this attribution.
- **Soak and browser session, first attempt** (not in the proof): started beside six short-boot lanes whose CPUs were the SMT siblings (n and n+20) of the soak's CPUs and of the browser
  session's: 3 340 commands an hour, churn fs median 16.9 s, worst 42 s. Both long runs were restarted with idle siblings; their numbers are in the table. Note
  for the next team: on this host the siblings of a pinned long run must stay idle.
- **KVM boots**: `[aslr]` FAILED (BUG-088 partial) and `GBIG` at 1 GiB as in 11d and d2. No other FAILED line.
- **TCG**: the FAILED lines of the 512 MiB configurations (`udrv-net`, `conn`, `churn-*`, `app-policy`) and `GBIG`/`aslr` at 1 GiB are in the baseline too; `[ap-user]` FAILED is new in the
  three 512 MiB smp 4 configurations (not launched, arena memory, known from D2); `[churn-forkthreads]` once, BUG-275.
- **q35**: `tpm` and `tpmcrb` skipped (no `swtpm` on the R630; nothing installed); the leg list is the 13 of chrome-fix190 (`nvmeroot` and `ahciroot` are not part of it).
- **gui-smoke** ran with its default network (`none`) except the `--net e1000e` run.
- The R630 ran at most about 30 guests at a time, all pinned; the machine was quiet for the second boot-time series and the second keylat series only after the soak had ended.
- `toolchain/eupkg/keys/dev.key` was a symlink for the builds only and is removed again; no `.sig` file or `dev.pub` is in the commit. `userland/*.elf` copied from the main checkout (45, as in wt-reg).

## 4. Extra images built for the bisects (not kept)

Self-test images of `c70dddf` (BUG-250), `0cf75e6` (BUG-251), `f42be2a` (BUG-252) for the power leg and the fuzz A/B; of `eedd5bf`, `875eb9e`, `0018719`,
`80c028d` for the AP bisect; the browser image of `3ecde9e` for keylat and starts. All removed from .16 and the R630 at the end.

## 5. What was not done

- No fix of any finding (rules: no code changes); the causes of BUG-270 (print queue) and BUG-271/272 (park wait) are from reading the code plus the bisects.
- The `swtpm` legs, the cause of BUG-274 and of the BUG-273 margin, and BUG-275 (not reproduced).
