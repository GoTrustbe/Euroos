# Re-validation of `feature/fix-d2` (release candidate, 2026-11-15/16)

Release candidate `feature/fix-d2` `732e5f5` (D2 fixes for BUG-270 to BUG-274 and BUG-182), worktree `/opt/euroos/wt-reval` on .16,
branch `feature/revalidate-2026-11e`. Not pushed. Images of `732e5f5` built with `scripts/build.sh` (self-test `bc6cf115...`,
public `8b390694...`, browser = public with `ui.tour=off;update.check=off` `06a775af...`); the public image of this build stays on the R630 as
`/srv/euroos/val-runs/img/v2-pub.img`. References: the public release 2026.11.14 = code `3ecde9e` (built here as self-test, public and browser image, "b0")
and the candidate that failed validation, `06e913e` (code `48dfd05`, "v1"; its numbers are the logs and results of `../validate-2026-11e/`, which are
still on the R630 in `/srv/euroos/val-runs`, plus its public image booted again for boot time and `[fair]`). R630 run directory `/srv/euroos/reval-runs`;
the drivers are in `drivers/`. Same scripts and methods as `../validate-2026-11e/` and `../fix-d2/`.

CPU layout (SMT sibling of CPU n is n+20, checked with `lscpu -e`): no two measuring guests on siblings of each other. The soak ran on CPUs 30-33 and the
browser session on CPUs 0-1 with their siblings idle; the keylat, boot-time and `[fair]` runs have idle siblings and tmpfs work directories; the 100 short
boots ran with both siblings of every core busy (the BUG-273 load) on cores that are not siblings of a soak or session CPU.

## 1. Verdict

**Klaar voor release: ja.** Every criterion of the list is met. The three D2 regressions of the failed candidate are gone (BUG-270, BUG-271/272, BUG-273,
BUG-274 and BUG-182 all measured), no result is worse than `06e913e` or `3ecde9e`. Points that are close to a limit or that need a remark, none blocking:
hammer p50 6.0 ms is exactly on its limit of 6 ms (5.5 ms on `06e913e`), the browser session memory verdict reads "NOT flat" (slope 0.6 % of the pool per hour, far
under the 5 % limit; BUG-281), one site step of the 2-hour session failed (OpenStreetMap "content still changing"), and `[fair]` is in its known 47-52 % band.
New tracker entries: BUG-280, BUG-281, BUG-282.

## 2. Results

| Test | Result `732e5f5` | `06e913e` (failed candidate) | `3ecde9e` (release 2026.11.14) |
|---|---|---|---|
| 1. `cargo test` (default members, as gate.sh) | 188 binaries, **1 381 passed, 0 failed**, 1 ignored (`cargo-test-summary.txt`) | same | - |
| 2. KVM boots 1/2/4/8 GiB, 2 vCPU, HD-Audio (`kvm-selftest-boots.txt`, `comparison-kvm.txt`) | desktop 4/4, 0 crash lines, 0 inversions; FAILED only `[aslr]` (BUG-088, known) and the two `GBIG` at 1 GiB; markers-compare **0 worse** at all four sizes against both | 0 worse | 0 worse (`[aslr]` has no marker there) |
| 2b. `[kwork]`, APs | `[kwork]` OK in all boots; 4 vCPU: 3 APs, 4 workers, **work-stealing=yes -> OK**; public image 4 vCPU `[smp] 3/3 AP(s) idle in hlt, taking kernel work` | 0/3 idle, `[kwork]` "no APs accepting work" | 3/3, OK |
| 3a. TCG matrix of 10, restrict=on (`tcg-summary.txt`) | **10/10 desktops**, 0 watchdog, 0 NMI, 0 fault lines, 0 FROZEN, 0 inversions; `[churn-forkthreads]` 40 of 40 in `combined-smp1-iommu-1g` (BUG-275 not seen); markers-compare against `06e913e`: **0 worse** in all 10 | 10/10, one intermittent worse | - |
| 3b. q35 metal matrix, KVM, restrict=on (`q35-matrix.txt`) | **13 of 13 PASS**: base, nvme, ahci, e1000e, hda, usb, usbhub, usbaudio, usbnet, hwprobe, scan, **power**, printer (printer run alone after the harness stopped at the missing `swtpm`); `tpm`/`tpmcrb` skipped (no swtpm on the R630). The harness default list also has `nvmeroot`/`ahciroot`: FAIL, as on `3ecde9e` (BUG-282, not part of the 13) | 12/13 (power FAIL) | 13/13 (chrome-fix190) |
| 4. gui-smoke, public image, KVM 4 vCPU 4 GiB (`gui-smoke-*.txt`, `comparison-public.txt`) | default **67/67**; `--soak 35 --seed 20261114` **78/78**; `--input virtio` **67/67**; `--net e1000e` (2 vCPU) **67/67**; `smpuser=on` smp 4 **67/67** | 67/67, 78/78 | 3ecde9e public image 67/67; markers-compare 0 worse (only new `[root-heap]`, `[ap-parity]` OK) |
| 5a. `wizard-install-test.py` | **15/15** | 15/15 | - |
| 5b. 10 installs side by side on 8 CPUs (`installs-side-by-side.txt`) | **10 of 10 OK**, 20-26 s | 10/10, 7-29 s | - |
| 5c. `ro-install-test.py` plain, inject, ro0 (self-test image) | **all 14 checks PASS** (the "verdict FAILED" line is the expected result of the inject mode) | 14/14 | - |
| 6a. boot time, public image, KVM 2 vCPU, pin 18-19, 12 triples alternating (`boot-time.txt`) | **median 2.61 s** (2.25-3.02), limit 2.8 s | 3.05 s (2.86-3.44), same session; validation 3.00 s | 2.66 s (2.24-3.00) |
| 6b. 100 short boots, `[fuzz] Linux-ABI`, both siblings of every core busy (`short-boots.txt`) | **108 boots, 0 `[fuzz]` FAILED, 0 crash lines**, CSPRNG seeded 108, `[3d8]`/`[3d7]` OK 108; `[fuzz]` after 35.0 s median, 39.6 s max | 21 of 108 FAILED | validate-2026-11d 0/88 |
| 6c. `rename-persist.py` | **24/24** | 24/24 | - |
| 7. loadtest soak 2 h, 16 GiB, 4 vCPU (`r630-soak-summary.txt`) | **10 699 commands, 5 133 an hour**, 0 errors; churn fs worst **10.96 s** (limit 12), churn proc worst 3.4 s, `[churn-proc]` 7 889 verdicts 0 FAILED; hammer 740 922 requests 0 errors, **p50 6.0 ms** (limit 6, on the limit); **0 watchdog**, 0 panic/NMI/FROZEN, 0 inversions (`[oom]`/`[spawn-refused]` 3 each are the `[memt]` self-test, as on `06e913e`) | 10 668, 5 119 an hour, churn fs worst 11.5 s, p50 5.5 ms | - |
| 8a. Chromium 20 starts, smp 2 (`chrome-starts20.txt`) | **20/20, 0 FROZEN** | 19/20 and 20/20, 3 FROZEN in about 55 boots | 20/20, 0 FROZEN beside it |
| 8b. sites, 3 rounds, smp 2, banners clicked by the harness | **PASS 36/36**, 0 aborts, 0 refused forks, 0 `[oom]`, 0 FROZEN | 36/36 | - |
| 8c. interaction | **PASS 18/18** | 18/18 | - |
| 8d. local VP9+Opus video with sound (`chrome-localvideo.txt`) | picture advances (28 of 28 screendumps), **12 of 12** five-second windows with sound (RMS 1 158); 16/21 steps = H.264+AAC failing by design | 12 of 12 | - |
| 8e. Wikimedia 480p VP9 + Opus, 2 x 90 s | **PASS**: 22/22 screendumps differ in both rounds, sound 17 of 18 windows in both | 16 and 17 of 18 | - |
| 8f. keylat, browser image, tmpfs, quiet, paired (`keylat.txt`) | smp 4 median ms: **250 247 219**; smp 2: **237 271 293** | 409-428 (smp 4), 398-416 (smp 2) | smp 4: 316 311 272; smp 2: 305 339 347: **faster in 6 of 6 pairs** |
| 8g. one 2 h session at 8 GiB, `chrome-mem-soak.py --local-video-dir` (vp9 only) (`chrome-soak2h.txt`, `-curve.txt`) | 11 rounds in 7 398 s, **178/179 steps**; the one failed step: `site openstreetmap` "content 274 colours, ... still changing" (no kernel line near it); 0 kills, 0 aborts, 0 refused forks, 0 `[oom]`, 0 FROZEN; heap flat (123 to 124 MiB, +0.8 MiB/h); in use +20.3 MiB/h (0.6 % of the pool an hour); demand pool free 2 469-2 471 MiB at the end of rounds 10 and 11; mem-curve verdict **"NOT flat"** only because 40 MiB (1.2 %) had no owner at the worst moment (BUG-281) | 179/179, +13.3 MiB/h, "flat" | - |
| 9. `[fair]` first round, hogtest 5 + churn 8, 4 vCPU (info; `fair.txt`) | public image, 10 quiet boots: **47-52 %, median 50 %**; self-test image 45-46 % (ring-3 demo spinners beside it, 1.5 s a round: not comparable, 10 boots quiet and 10 beside the soak) | public image 45-51 % (5 boots) | no `[fair]` self-test yet |
| 10. `smpuser=on`, smp 4 (info) | gui-smoke **67/67**; sites **36/36** | - | - |

## 3. Deviations and explanations

- **First soak discarded** (`soak/` on the R630, not in the table): it ran beside the install, TCG and q35 lanes, which write the same disk. At t = 1 240 s the guest saw
  `[blk] slow flush ... 354 ms` and `slow write ... 366 ms`, one `churn fs` took 36.4 s and the deadman printed `[3g2-wire] WATCHDOG TRIPPED - main loop hung`. 5 148 an hour, p50 6.0 ms.
  The soak was repeated from scratch with nothing disk-heavy on the host (second run, in the table: 0 watchdog, churn fs worst 10.96 s). The disturbed run is tracked as BUG-280.
- **Browser session**: the first 2 h run (9 rounds in 7 988 s) pointed the harness at `/srv/euroos/harness-runs/vid`, which also holds the H.264 clip: its 45 failed steps are all
  `local video h264+aac` (by design); the 219 other steps were all OK, 0 aborts/oom/FROZEN, slope +16.6 MiB/h, also "NOT flat". The serial log of that run was lost when its work directory was cleaned; the numbers are
  from the notes taken at the time. The run in the table uses a clip directory with `vp9.webm` only, as the validation did, and `--local-port 18782`.
- **Harness slips, repeated and not counted**: `ro-install-test.py` needs the self-test image (first attempt with the public image: markers missing); `--only localvideo` and the session both listen on port 18780 (the localvideo run
  was repeated with `--local-port 18781`); `wmvideo.py` defaults to a 360p Wikimedia file that no longer plays (0 of 22 screendumps differ, `runs/wm360-failed.out`), the 480p URL of the validation was used.
- `[kwork]` on the 2 vCPU boots says "work-stealing=n/a (one AP)"; the work-stealing proof is the 4 vCPU boot (3 APs, yes).
- **q35**: the harness default list has 17 legs and stops at `tpm` (no `swtpm`); the printer leg was run alone. `nvmeroot` and `ahciroot` are not in the 13; they fail in the same way on the 3ecde9e self-test image (BUG-282).
- **TCG**: the FAILED lines of the 512 MiB configurations are the same set as on `06e913e` (markers-compare 0 worse).
- Wikimedia sound 17 of 18 windows is the same as the validation (16 and 17); the OpenStreetMap step is a live site.
- `toolchain/eupkg/keys/dev.key` was a symlink for the builds only and is removed; no `.sig` and no `dev.pub` in the commit. `userland/*.elf` copied from the main checkout (45).

## 4. What was not done

- `tpm` and `tpmcrb` legs (no `swtpm`, nothing installed).
- A third browser session to see whether the 0.6 % per hour slope (BUG-281) is the same on `3ecde9e` (not measured there).
- Cause of the 36 s stall of the disturbed soak (BUG-280): only correlated with the host disk stall.
