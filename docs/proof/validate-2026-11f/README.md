# Validation of sprint 2026-11f (`feature/11f-integ` `00a6459`, 2026-11-16)

Worktree `/opt/euroos/wt-11f-val` on .16, branch `feature/validate-2026-11f` from `00a6459`. Not pushed. Reference = the public release 2026.11.16
(code `e4b8ba4`), the real download from the web root (`euroos-x86_64-uefi.img.gz`, raw sha256 `4f15ad0e...`); its self-test image was built from `e4b8ba4`
in `/opt/euroos/wt-11f-val-b0` (`b0-st`). Browser images = public image + `ui.tour=off;update.check=off` written to the ESP (so the reference browser image has
the bytes of the real download). R630 run directory `/srv/euroos/v11f-runs`; drivers in `drivers/`; the harness is the one of `00a6459` for every cell.
The public image of this build stays on the R630 as `/srv/euroos/val-runs/img/v3-pub.img` (sha256 `cfc4f2f1...`).
CPU layout: no two measuring guests on SMT siblings (sibling = cpu+20); timing runs on tmpfs with idle siblings; the soak (CPUs 30-33, siblings idle) had no
disk-heavy lane beside it; CPUs 18-19 and 38-39 were left to another team from the middle of the run.

## 1. Verdict

**Klaar voor release: ja.** No blocking point. Every criterion of the list is met. Remarks, none blocking: hammer p50 is 6.0 ms, exactly on its limit (as in 11e);
the 2 h Chromium session lost 4 OpenStreetMap steps of 179 ("content still changing", live site, BUG-341); one `--input virtio` gui-smoke run lost the two
BUG-057 notification-list steps and passed 67/67 on repeat (BUG-340, also not reproduced on the reference).

## 2. A. Key latency: is the merge faster than 2026.11.16?

**Yes. The merge is faster by about 50 ms (fbread) and is under 100 ms in every run; team A was right, the integration team measured something else.**
Method: `chrome-smoke --only perf` (30 keys; one run reports both methods, fbread and the old screendump poll), browser images, smp 4 and smp 2, tmpfs, siblings
idle, 6 rounds, all four guests (reference/merge x smp 4/2) at the same time, CPU sets swapped every round, merged harness for all. Per run: `keylat-pairs.txt`.

| median, 6 runs per cell (min-max) | 2026.11.16 public | merge `00a6459` |
|---|---|---|
| smp 4, fbread (verdict method) | 147 ms (131-154) | **95 ms (92-98)** |
| smp 2, fbread | 148 ms (145-159) | **91 ms (87-96)** |
| smp 4, screendump (old method) | 239 ms (226-248) | 158 ms (148-167) |
| smp 2, screendump | 240 ms (232-261) | 150 ms (137-165) |
| kernel `[keylat]` total, key IRQ to first paint (last 30 keys, all 12 runs) | 103 ms (99-111) | **76 ms (71-82)** |

Every merge run is under 100 ms (fbread 87-98); every reference run is over (131-159). Harness check: `git diff 4e681ba 00a6459 -- scripts/chrome-smoke.py` differs only in
`focus_omnibox` (Ctrl+L seen on the screen, then a click; used before typing the URL and before the perf load); the key typing, `fbread` and the median are unchanged.
Base image of the integration team: their images are gone, so it cannot be checked directly. My own rebuild of `e4b8ba4` (`kx*` runs, 3 x smp 4 and 3 x smp 2) reads 137-154 ms
(fbread), the same as the public download, so a build of `e4b8ba4` is slow with this harness; the "~95 ms for `e4b8ba4`" of the integration team therefore did not come from
`e4b8ba4` code (most likely their "base" image already carried the key-latency fixes; not verifiable). The rebuilt image is not byte-identical to the download (9 195 bytes differ in the kernel and
the loader, `repro-and-base-image.txt`; the release build environment was not reproducible here), but its latency equals the download's.

## 3. B. Results

| Test | Merge `00a6459` | 2026.11.16 / reference |
|---|---|---|
| 1. `cargo test` | 188 binaries, **1 381 passed, 0 failed**, 1 ignored (`cargo-test-summary.txt`) | same |
| 2. KVM boots 1/2/4/8 GiB, 2 vCPU, HD-Audio (`kvm-boots.txt`, `comparison-kvm.txt`) | desktop 4/4, 0 crash lines, FAILED only `[aslr]` (BUG-088) and the two `GBIG` at 1 GiB; markers-compare **0 worse** at 1/2/4/8 GiB and at 4 vCPU; 4 vCPU: 3 APs, `[kwork]` work-stealing OK, `[xlock]` OK, `[fuzz]` OK | same FAILED set |
| 3a. TCG matrix of 10, restrict=on (`tcg-summary.txt`) | **10/10 desktops**, 0 watchdog, 0 NMI, 0 FROZEN, 0 fault lines; `[churn-forkthreads]` 40/40 in the four 1 GiB configurations; the 3 `[churn]` FAILED lines of the six 512 MiB configurations are identical on the reference (checked on two configs) | 3 FAILED in the same two configs |
| 3b. q35 matrix, KVM, restrict=on, 15 legs (`q35-matrix.txt`) | **ALL LEGS PASS** (base, nvme, ahci, e1000e, hda, usb, usbhub, usbaudio, usbnet, hwprobe, scan, power, printer, nvmeroot, ahciroot) | - |
| 4. gui-smoke, public image, KVM 4 vCPU 4 GiB (`gui-smoke.txt`) | default **67/67**; `--soak 35 --seed 20261114` **77/77**; `--net e1000e` **67/67**; `smpuser=on` smp 4 **67/67**; `--input virtio` first run 65/67 (BUG-057 notification-list Esc and click), repeat **67/67** (BUG-340) | default 67/67; `--input virtio` repeat 67/67 |
| 5a. `wizard-install-test.py` | **15/15** | - |
| 5b. 10 installs side by side on 8 CPUs | **10 of 10 OK**, 6-24 s | - |
| 5c. `ro-install-test.py` plain / inject / ro0 (self-test image) | all checks PASS (the "install verdict FAILED" line is the expected result of the inject mode) | - |
| 6a. boot time, public image, KVM 2 vCPU 2 GiB, pin 8-9, quiet host, 12 pairs alternating (`boot-time-quiet.txt`) | **median 2.63 s** (2.24-2.94), limit 2.8 s | 2.64 s (2.24-3.01) |
| 6a'. first boot-time series, beside the soak and 4 other lanes (`boot-time-beside-soak.txt`) | median 2.88 s (2.53-3.05): not valid, host loaded; repeated quiet above | 2.69 s (2.50-3.03) |
| 6b. 108 short boots, `[fuzz] Linux-ABI`, both siblings of every core busy (`short-boots.txt`) | **108/108 end at the marker, 0 `[fuzz]`/`[churn]` FAILED, 0 crash lines, 0 wedges**, CSPRNG seeded 108; `[fuzz]` after 34.0 s median, 39.5 s max | - |
| 6c. `rename-persist.py` | **24/24** | - |
| 7. loadtest soak 2 h, 16 GiB, 4 vCPU (`soak-summary.txt`, `soak.csv`) | **10 836 commands, 5 201 an hour**, 0 errors; churn fs worst **11.3 s** (limit 12), churn proc worst 3.5 s, `[churn-proc]` 7 987 verdicts 0 FAILED; hammer 716 755 requests 0 errors, **p50 6.0 ms** (limit 6, on the limit); **0 watchdog**, 0 panic/NMI/FROZEN, 0 inversions (`[oom]`/`[spawn-refused]` 3 each = the `[memt]` self-test) | - |
| 8a. Chromium 20 starts, smp 2 | **20/20, 0 FROZEN** | - |
| 8b. sites, 3 rounds, smp 2, banners clicked by the harness | **PASS 36/36**; slowest desktop-loop console answer 865 ms (no 1.5 s stall) | - |
| 8c. interaction | **PASS 18/18** | - |
| 8d. local VP9+Opus video with sound | picture advances 28 of 28 screendumps, currentTime 2.96 to 61.46 s in 60.8 s, **12 of 12** five-second windows with sound; 16/21 steps = H.264+AAC failing by design | - |
| 8e. Wikimedia 480p VP9 + Opus, 2 x 90 s (`wmvideo.py`) | **PASS** | - |
| 8f. keylat | section 2 | section 2 |
| 8g. one 2 h session at 8 GiB, local VP9 (`chrome-soak2h.txt`, `chrome-soak2h-curve.txt`) | 11 rounds in 7 476 s, 175/179 steps; the 4 failed steps are `site openstreetmap` "content 273 colours ... still changing"; 0 aborts, 0 refused forks, 0 `[oom]`, 0 FROZEN; **memory verdict flat** (in use +19.4 MiB/h = 0.6 % of the pool per hour, 7 MiB without an owner at the worst); slowest console answer 607 ms | - |
| 9. `[fair]` first round, hogtest 5 + churn 8, 4 vCPU, 10 boots (`fair.txt`) | self-test image **47-49 %** (median 48 %) | reference self-test image 44-46 % |
| 10. `smpuser=on` smp 4 (info) | gui-smoke **67/67**, sites **36/36** | - |
| 11. reproducible image | `scripts/build.sh release` twice: identical sha256 `f9f0dbcb...`; `image` twice: identical `cfc4f2f1...` (`repro-and-base-image.txt`) | - |

## 4. Deviations and explanations

- Boot time: the first series ran beside the soak and four other lanes (loaded host, kernel entry 2.0-2.2 s instead of 1.5-1.8 s on both images, reference 2.69 s too); it is kept in the proof
  but the criterion was judged on the quiet repeat (6a).
- Harness slips, not counted: the wait pattern of `phase2.sh`/`klat.sh` did not match "FAIL (n step(s)) - a/b steps ok" lines (the Chromium lane waited for the localvideo run for ever; the key-latency
  rounds 2-6 were started again by hand, the Wikimedia run was started by hand); `wmvideo.py` needs `--url` (480p URL of the earlier validations); `short-boots.sh`, `soak_compare.py`, `wmvideo.py` are not in the repository and came from `val-runs/scripts`.
- OpenStreetMap: the step fails when the live map keeps animating; 4 of 11 rounds in the session, 0 of 3 in the sites run and in the smpuser sites run (BUG-341, harness/live site).
- Key latency: the reference browser image is the real download with the same ESP options, not a rebuild; the rebuild gives the same numbers.
- `tpm`/`tpmcrb` legs were not part of the 15 legs (no `swtpm` on the R630).
- `toolchain/eupkg/keys/dev.key` was a symlink for the builds only and is removed; no `.sig`, no `dev.pub` in the commit. `userland/*.elf` copied from the main checkout (45).

## 5. Not done

- No `tpm`/`tpmcrb` legs; no repeat series of BUG-340 (one occurrence).
- The exact origin of the integration team base image (their images no longer exist).
