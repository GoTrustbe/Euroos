# Validation of the 2026-11g candidate (`feature/11f-integ2` `368919c`, 2026-11-19)

Worktree `/opt/euroos/wt-11g-val` on .16, branch `feature/validate-2026-11g` from `368919c`. Not pushed. Reference = the published
2026.11.17 (download `euroos-x86_64-uefi.img.gz` sha256 `56c5f3be...`, checked; raw image sha256 `ffa37753...`). Its code is `b30adb6`
(= the validated `00a6459` plus docs); a self-test image of `b30adb6` was built in `/opt/euroos/wt-11g-ref` for the marker comparisons
(`b0-st`). Candidate images were built as for a release (`scripts/build.sh image` and `release`, own `target/` of the worktree).
R630 run directory `/srv/euroos/val-runs/g`; drivers in `drivers/`; the harness is the one of the candidate tree for every cell
(`short-boots.sh`, `wmvideo.py`, `soak_compare.py`, `analyze_soak.py` are not in the tree and came from `val-runs/scripts`).
Naming: `m` = candidate, `b0` = reference 2026.11.17. Candidate image (public type) sha256 `c88fe8103c8cc4a962600a928f3031ba4c7e31425784cc7aa5f616acfe8756a3`
(kept as `/srv/euroos/val-runs/img/g-cand.img`); self-test image `8b578dec...`; reference self-test `3aea1d50...` (`images.txt`).
Browser images = public-type image + `ui.tour=off;update.check=off` on the ESP (user-net, no hostfwd to the outside).

## 1. Verdict

**Ready to publish: yes.** No ring-0 fault, watchdog, panic, freeze, `[irqlock]` line or data loss in any run (KVM, TCG, q35, installs, three
1 h browser sessions, two 1 h load soaks side by side, 60 short boots). Every number that matters is equal to or better than 2026.11.17
(boot time, key latency, soak throughput, `[fair]`, memory); no figure is more than 10 % worse. Four things are not clean and are listed in
section 4: one `[clock-drift]` FAILED in a loaded KVM boot (not reproduced in 8 repeats), two new self-tests that FAIL at 512 MiB under TCG,
documentation that contradicts the tracker (fix before the release), and what was not run (the update client against a channel). None blocks.

## 2. Results (candidate / reference 2026.11.17)

| # | Test | Candidate | Reference | File |
|---|---|---|---|---|
| 1 | `cargo test` | 188 binaries, **1396 passed, 0 failed**, 1 ignored | - | `cargo-test-summary.txt` |
| 2 | KVM boots 1/2/4/8 GiB x 1/2/4 vCPU (12 each) | **12/12 desktops, 0 crash lines**. FAILED lines: 1 GiB: 2 (`GBIG`, known) x3; 2/4/8 GiB: 0 (one exception: 8 GiB 1 vCPU, 3 lines = `[clock-drift]`, section 4.1). `[aslr]`, `[aslr-ind]`, `[aslr-exec]`, `[aslr-fork]`, `[aslr-phys]`, `[arena-*]`, `[fork-cow-write]`, `[fork-fault-wait]`, `[vdso-ro]`, `[vdso-mono]`, `[swap-irq]`, `[elf-bias]` OK in 12/12 (`tracker-markers.txt`) | 12/12 desktops, 0 crash; FAILED: `GBIG` x2 at 1 GiB and `[aslr]` (BUG-088) in every boot | `kvm-boots.txt` |
| 3 | boot options (6 x KVM boot, gui-smoke, Chromium 5 starts + 1 sites round) | all 6 **boot, 67/67, 5/5, 14/14** | - | section 3, `bootopts.txt`, `gui-smoke.txt`, `chrome.txt` |
| 4 | entropy | equal to ASLR.md 12.2 in every cell, section 5 | - | `entropy.txt`, `chrome-entropy.txt` |
| 5a | TCG matrix 10 legs, restrict=on, 512 MiB / 1 GiB | **10/10 desktops**, 0 watchdog, 0 NMI, 0 crash, 0 frozen; 6 legs add `[swap-irq]` + `[vdso-mono]` FAILED at 512 MiB (4.2) | 10/10 desktops, same | `tcg-m.txt`, `tcg-b0.txt` |
| 5a' | TCG user/kernel legs at 2 GiB | **6/6 desktops, `[vdso-mono]` and `[swap-irq]` OK**, FAILED lines 5/5/5/5/1/1 (`[udrv-*]`, `[conn]`: no network behind `restrict=on`) | 2 legs (smp 4): 6 and 2 FAILED = the same + `[aslr]` | `tcg-m2g.txt`, `tcg-b02g.txt` |
| 5b | q35 matrix, KVM, 15 legs (tpm legs not run: no swtpm) | **ALL LEGS PASS** | - | `q35-matrix.txt` |
| 5c | 60 short boots, 12 lanes on 12 cores and siblings | **60/60 end at `[fuzz] Linux-ABI`, 0 FAILED, 0 crash**; marker after 32.7 s median, 38.9 s max | (11f: 34.0 / 39.5 s) | `short-boots*.txt` |
| 6 | gui-smoke, KVM 4 vCPU 4 GiB | **67/67** (default and the five boot options) | - | `gui-smoke.txt` |
| 7a | wizard install | **15/15** | 15/15 | `installs.txt` |
| 7b | 10 installs side by side | **10/10 OK**, 7-21 s | - | `installs.txt` |
| 7c | `ro-install-test` plain / inject / ro0 | all checks PASS (the "install verdict FAILED" line is the expected result of inject) | - | `installs.txt` |
| 7d | `rename-persist` | **24/24** | - | `installs.txt` |
| 7e | update path, disk level (4.4) | disk installed by the 2026.11.17 installer, candidate kernel in the slot partition: loader verifies it, boots `20261119`, sign-in with the 11.17 account, files, scrub, restart, power cut **23/23** | same disk with 11.17: 23/23 | `update-path.txt` |
| 8a | Chromium 20 starts | **20/20** (5/5 steps each) | 20/20 | `chrome.txt` |
| 8b | sites 3 rounds | **36/36** | 36/36 | `chrome.txt` |
| 8c | interaction | **18/18** | - | `chrome.txt` |
| 8d | video VP9 + Opus (local, clip map without `h264.mp4`) | in 3 soaks x 5 rounds: picture advances 28/28 screendumps, sound in 12/12 five-second windows, `currentTime` +58.5 .. +59.5 s in 60.3 .. 61.2 s, no media error; Wikimedia 480p VP9+Opus 2 x 90 s PASS | soak ref: same | `chrome-soaks.txt`, `chrome.txt` |
| 8e | 3 chrome-mem-soaks of 1 h (8 GiB, 2 vCPU) | **122/123, 121/123, 123/123 steps**; the 3 failed steps are all `openstreetmap` "content ... still changing" (BUG-341); **0 `[irqlock]`, 0 frozen, 0 watchdog, 0 crash, 0 `[oom]`, 0 "mmap window full"**; 5 rounds each (3948-4029 s) | ref soak: 123/123, same counters | `chrome-soaks.txt` |
| 8f | loadtest soak 1 h, 16 GiB, 4 vCPU | twice: 5468 and **5864 commands** (5045 / 5398 an hour), 0 not ok, hammer 370 633 and 385 406 requests, 0 errors, **p50 6.1 / 5.8 ms**, 0 crash/watchdog/irqlock | 6072 and 5740 commands (5601 / 5293 an hour), hammer p50 5.8 / 6.0 ms | `loadtest-soaks.txt` |
| 10 | clock drift 30 min, KVM | section 6 | | `clock.txt` |

## 3. Boot options (task 3; `BOOT-OPTIONS.md` against the log)

Per option: self-test KVM boot 2 GiB / 2 vCPU (`[bootopt]` lines, `[aslr]`), gui-smoke, Chromium 5 starts and one sites round (all with the option on
the ESP of a copy of the candidate image; Chromium images also `ui.tour=off;update.check=off`).

| Option | `[bootopt]` lines | Measured (KVM boot; Chromium launch) | Doc says | Verdict |
|---|---|---|---|---|
| `aslr=on` (default) | `on`, `random`, `on` | every region 27-34 bits, section 5 | same | as documented |
| `aslr=off` | `off`, `random`, `on` | **0 bits** everywhere (boot and Chromium: exe, ld.so, arena, segments, brk, stack, floor, vDSO); `[aslr]` shows "off" (skipped by design) | same layout, always the same addresses | as documented |
| `aslr=legacy` | `legacy`, `identity`, `off` | exe, ld.so, vDSO 0 bits; brk 8, stack 7-8, floor 19-20 (Chromium), mmap 16 | layout of "2026.11.16 and earlier" | as documented, **but the doc names the wrong release** (7.2) |
| `aslr.arena=fixed` | `on`, `fixed`, `off` | exe/floor/vDSO random (27), brk 8, stack 8, ld.so 0: one block at 8 TiB | always at 8 TiB, rest random | as documented |
| `aslr.arena=identity` | `on`, `identity`, `off` | same bits as fixed (exe 27, floor 26, brk 8, stack 7); `[aslr-phys]` addresses at their physical place | as 2026.11.16 | as documented |
| `aslr.split=off` | `on`, `random`, `off` | one block, random place: exe 29, ld.so 28, brk 28, stack 29, arena 20; distances inside the block 9-10 bits (ld.so-brk, brk-stack, ld.so-stack); `[aslr]` OK, `[aslr-ind]` FAILED (by design: the S5 layout) | S5 layout | as documented; note that the self-tests `[aslr-ind]`/`[aslr-exec]`/`[aslr]` print FAILED under `legacy`, `fixed`, `identity` and `[aslr-ind]` under `split=off` (they judge the default layout; the docs do not say so) |

All six: desktop, gui-smoke **67/67**, Chromium **5/5 starts** and **14/14** sites steps (incl. OpenStreetMap), 0 crash lines. `aslr.split` shows `off` when the
placement is not `random` (`fixed`, `identity`, `legacy`) as ASLR.md 12.3 says.

## 4. Regressions and deviations from the reference

### 4.1 `[clock-drift]` FAILED once at 8 GiB, 1 vCPU, loaded host (LOW, not reproduced)
One of 7 candidate boots at 8 GiB / 1 vCPU (the one that ran beside the first heavy lane): `[clock-drift] rest: clock +500 ppm (+5006 us), tick count +869 ppm`,
`late: clock +499 ppm` -> FAILED (the clock sat on the 500 ppm slew limit of the time base during the 10 s test window). **8 repeats on a quiet host (8 GiB, 1 vCPU,
pin 12): 8 of 8 OK**; the other 11 KVM boots, the 10 TCG legs (512 MiB and 2 GiB) and all 30 s-window checks OK. Reference: test does not exist (its clock drifts
-1730 ppm, section 6). Severity LOW (a 5 ms clock offset in the first seconds after boot under host load); closest known bug: BUG-400 (the time base converges
from the 10 ms calibration; the test window is short). `clock.txt`, `kvm-boots.txt`.

### 4.2 `[swap-irq]` and `[vdso-mono]` FAILED at 512 MiB under TCG (test side, LOW)
In the 6 user/kernel legs of the 512 MiB matrix the candidate prints `[vdso-mono] ... clock frame shared by 0 of 0 child calls ... exec'd child has a vDSO: not measured -> FAILED`
and `[swap-irq] ... 0 of 1 runs ok`. Both are new self-tests (absent in the reference) that need glibc fork children; at 512 MiB no glibc child runs on **either** image (`[churn-fork]`, `[churn-proc]`,
`[churn-thread]` exit=-1 on both). At 1 GiB (storage/combined legs) and at 2 GiB (6 legs) both are OK and the 3 clock lines are OK. Not a regression of the kernel; a test that
reports FAILED instead of SKIPPED when it cannot start its children. The integ2 proof ran this matrix at 2 GiB. `tcg-m.txt`, `tcg-m2g.txt`.

### 4.3 Chromium "first page settled" metric (not a regression)
`chrome-smoke --starts 20`: candidate 15 of 20 starts report "painted and settled after ~53 s" (the harness caps `settle` at 40 s), 5 report 14.0-14.9 s; reference 20 of 20 report 14.4-15.7 s.
The stages before it are identical (launched 3.1 s, window on screen 3.4 s, X connection ~5.2 s, renderer ~8.5 s). The screenshot of a 53 s start shows the test page painted with the blinking
text caret in field A (the screen never stays quiet for 2.5 s); the screenshot of a reference start at 14.8 s shows a blank white Chromium window (quiet, but not yet painted). So the reference "14.9 s" is a
blank window; the candidate paints the page and the caret keeps the screen changing. The 5/5 steps (page input, omnibox, menu) pass on both. The same split (53 s / 14 s) occurs with every boot option, `off` and `legacy` included, so it is not ASLR. A paint
time that does not depend on the caret is not measured by this harness; the load of the six sites (3 rounds, `chrome-timing.txt`) is equal or faster than the reference (medians 2.3-11.8 s against 3.9-21.4 s; the
reference sites ran while other lanes were busy, treat as equal).

### 4.4 Update path (what was and was not run)
The published 2026.11.17 client only talks to `euro-os.eu:443` and `euroupdate fetch/check/apply` is root-only (root is locked; the wizard user is not uid 0), so the client could not be run against a local channel.
What was run: (a) the 2026.11.17 public image installs itself on a blank disk (wizard keys); (b) the candidate kernel and its build signature (taken from the candidate image, not re-signed) are written into the slot partition
exactly as `write_image_to_slot` does (`drivers/slotwrite.py`); (c) the disk boots standalone: `[loader] kernel image from the slot partition (header + sha256 + Ed25519 verified)`, `running version 20261119`
(the untouched copy boots `20261117`), `slot A confirmed GOOD`; (d) `rename-persist.py` on both copies: **23/23 on both** (sign-in with the account the 11.17 installer made, 5 steps of file operations, scrub clean,
restart, power cut). The candidate's own `[upd3]` (verify-before-activate, tamper refused, staging) is OK in every boot; `[3e2]` (channel against a server) and `[g4]` need a server/disk and printed
"no server" / "not readable" as in every self-test boot. **Not run: `euroupdate fetch|check` end to end, A/B flip + rollback on a real update.** An earlier attempt with `[upd2]` showed that a self-test live image next to an installed disk
boots the installed disk's slot kernel (the loader prefers slot partitions on any disk), so `update-test.py` no longer exercises `[upd2]`.

## 5. Entropy (task 4) against ASLR.md 12.2

20 KVM boots (2 GiB, 2 vCPU, self-test image, `[aslr]` = 64 starts of `gaslrx`) and 20 Chromium starts (launch + 192 exec'd children, `[aslr-layout]`). Varying bits:

| Region | ASLR.md 12.2 | measured over boots / per boot | Chromium launch / exec |
|---|---|---|---|
| exe (PIE) | 30 | 30 / 30 | 30 / 30 |
| ld.so | 30 | 30 / 30 | 30 / 30 |
| stack | 29 | 29 / 29 | 29 / 30 |
| brk | 29 | 29 / 29 | 29 / 29 |
| small mmap (4 KiB) | 30 | 30 / 30 | |
| mmap 1 MiB, libc (floor) | 30 | 30 / 30 | 30 / 30 |
| vDSO | 27 | 27 / 27 | 27 / 27 |
| arena image/heap/stack segment base | 21 each | | 21, 21, 21 / 21, 21, 21 |
| exe-brk, brk-stack, exe-stack | 31, 31, 32 | 31, 31, 32 | 31, 31, 33 |
| exe-small mmap, stack-small mmap, stack-ld.so | 31, 31, 32 | 31, 31, 32 | |
| exe-mmap 1 MiB, exe-vDSO | 34, 31 | 34, 31 | exe-floor 32, exe-vdso 31 |
| ld.so-brk, ld.so-stack (Chromium) | 31, 32 | | 31, 32 |
| same segment: ld.so-exe, small mmap-brk | 12, 14 | 12, 14 | |

`[aslr]` OK 20/20 and `[aslr-ind]` OK 20/20; **0 of 192 exec'd children kept an address of the launched browser** (ASLR.md: 0 of 185). No deviation.

## 6. Clock (task 10), 30 min on KVM, slope against the host CLOCK_MONOTONIC (`vdrift.py`, first sample dropped)

| 30 min | candidate (vDSO clock) | reference 2026.11.17 (tick count x 10 ms; HPET beside it) |
|---|---|---|
| at rest (2 vCPU, siblings idle) | **+6.5 ppm** (+0.007 s; HPET +0.7) | **-1730 ppm (-2.8 s)**; HPET -5 |
| loaded (1 vCPU, busy sibling) | **-0.2 ppm** | **-1088 ppm (-1.8 s)**; HPET +0.2 |

The reference prints no vDSO clock in `uptime`, so its tick count is the clock it reads (BUG-400); the HPET is the control. `[vdso-mono]` OK in 12/12 KVM boots and in every TCG leg at >= 1 GiB (4 legs at 1 GiB, 6 at 2 GiB; 0 steps back);
`[clock-drift]` OK in 11/12 KVM boots (4.1), 10/10 TCG legs. 16 repeat boots at 8 GiB / 1 vCPU (8 + 8): candidate 8 x OK.

## 7. Performance against the reference (task 9, same methods as validate-2026-11f)

| Measure | Candidate | Reference | Difference |
|---|---|---|---|
| boot time to the desktop, KVM 2 vCPU 2 GiB, pin 8-9, quiet host, 12 pairs alternating | **2.53 s** (2.24-2.94) | 2.65 s (2.23-2.94) | -4 % |
| kernel entry | 1.77 s | 1.88 s | -6 % |
| key latency fbread (30 keys, 6 rounds), smp 4 | **47 ms** (45-47) | 97 ms (96-99) | **-52 %** |
| key latency fbread, smp 2 | **55.5 ms** (53-64) | 96.5 ms (91-99) | -42 % |
| key latency screendump method, smp 4 / smp 2 | 105 / 103 ms | 164 / 158 ms | -36 % / -35 % |
| `[fair]` hogtest 5 + churn 8, 4 vCPU, 10 boots, lowest task 0 share | 46-47 %, worst 1.4-1.5 s | 46-47 %, worst 1.4-1.7 s | equal (both print FAILED: the verdict threshold, not a change) |
| loadtest soak, side by side, same hour: commands an hour | 5398 | 5293 | +2 % |
| hammer HTTP p50 / requests | 5.8 ms / 385 406 | 6.0 ms / 379 966 | -3 % |
| churn fs median / churn proc median / sha256sum median | 8.5 s / 1.34 s / 431 ms | 8.6 s / 1.31 s / 440 ms | equal |
| first soak (not side by side: the candidate ran beside 3 browser soaks, the reference beside drift runs): commands an hour | 5045 | 5601 | -10 % (-9.9 %); churn fs median 9.6 s against 8.1 s. **Not a valid comparison**; repeated side by side above (+2 %) |
| Chromium window on screen / X connection / renderer after Enter | 3.4 / 5.2 / 8.5 s | 3.4 / 5.0 / 8.4 s | equal |
| memory (chrome-mem-soak round 5, 8 GiB guest): heap used / big / files / demand free | 123 MiB / 31-35 of 480 MiB / 43 MiB / 2532-2595 MiB | 123 / 34 of 477 / 43 / 2529 | equal |

No figure is more than 10 % worse once like is compared with like. Key latency halves (11f measured 95 ms for the merge; the candidate 47-56 ms with the same harness; the cause is not known to me:
not investigated, the proof dirs of integ2 do not name a key-latency change).
Note on the churn fs worst case: 23.5 s (candidate) and 28.2 s (reference) in the side-by-side soak, while installs and boots ran on the same disk; first soak: worst 10.8 s / 10.5 s. Equal on both sides.

## 8. Documentation and tracker (task 11)

Contradictions, **not corrected** (release owner decides):
1. `UPDATES.md` NEXT: "BUG-390 (open, under investigation)" and "BUG-400 (open, under investigation) ... Not fixed yet" - the tracker has both FIXED (BUG-390 `9692f11`, BUG-400 `f487cc7`) and
   the candidate shows the effect (section 6; 3 browser soaks without a hang). BUG-410 and BUG-430 (fixed) and BUG-420, BUG-431, BUG-440 (open) are not mentioned in NEXT.
2. `BOOT-OPTIONS.md` (`legacy`, `identity`) and `SECURITY-ASLR.md` ("since the release after 2026.11.16", "as in 2026.11.16 and earlier", step 3 and 4): the **published release is 2026.11.17** and it
   has the old layout (reference `[aslr]` FAILED in every boot: mmap 9, stack 8, PIE 0 bits; no `[vdso-ro]`, no `[arena-segs]`). `UPDATES.md` has no "What changed in 2026.11.17" section, and NEXT says the writable vDSO "was in 2026.11.16" (it is in 2026.11.17 too).
3. `UPDATES.md` NEXT "Version number and date: `NEXT`": to fill in at the release; `EUROOS_BUILD_VERSION` of this build is 20261119.
4. The self-tests `[aslr]`, `[aslr-ind]`, `[aslr-exec]` print FAILED under `aslr=legacy`, `aslr.arena=fixed|identity` and `[aslr-ind]` under `aslr.split=off` (they judge the default layout); `SECURITY-ASLR.md` and
   `BOOT-OPTIONS.md` say the options are "a way back" but not that a self-test boot shows FAILED lines with them.
Confirmed against measurement: `BOOT-OPTIONS.md` / `SECURITY-ASLR.md` (every option does what it says, section 3; "about 30 bits", "31 to 32 bits between two parts"; 4 MiB stack part not tested; "none of the Chromium runs logged `mmap window full`": 0 in 20 starts and 3 soaks);
ASLR.md 12.2 numbers (section 5); UPDATES NEXT facts: vDSO read-only (`[vdso-ro]` OK 12/12), clock does not step back (`[vdso-mono]` OK), "about 1 s per 15 minutes" is 1.6 s per 15 min at rest and 1.0 s loaded on the reference.
Tracker status against the evidence (`tracker-markers.txt`): BUG-350 (`[aslr-fork]`), 351 (`[fork-cow-write]`), 352 (`[fork-fault-wait]`), 353 (`[vdso-ro]`), 354 (`[aslr-exec]`, `[arena-move]`), 355 (exec'd child with a vDSO: Chromium
20 starts, 192 children), 371/380 (`[vdso-mono]` 12/12 KVM, TCG at 2 GiB), 400 (section 6), 430 (`[swap-irq]` 12/12 KVM; the TCG 512 MiB FAILED is 4.2): all consistent with FIXED. BUG-390 FIXED: three 1 h browser sessions and two 1 h load soaks with 0 hangs and 0 `[irqlock]`;
the original hang was seen once and never reproduced in a browser, so the proof stays the `[swap-irq]` self-test (the tracker says so). BUG-410: no run exercises it (chrome-boot image not part of the list). BUG-341 (OpenStreetMap) failed 3 steps in 2 of 3 soaks,
0 in the sites runs, 0 in the reference soak (known, not blocking).

## 9. Open bugs for the release notes
BUG-341 (OpenStreetMap step "content still changing", live site; 3 of 123 soak steps), BUG-420 (wall clock never adjusted after boot; REALTIME follows the monotonic clock), BUG-431 / BUG-440 (locks taken both ways: same class as BUG-390, found by reading, not changed),
BUG-370 / BUG-372 (`fstat` on a pipe, EFAULT on an untouched page of a writable file mapping), BUG-390 is **fixed in the candidate but present in 2026.11.17** (the tracker says so: same risk), BUG-306 / BUG-340 not seen in this run (gui-smoke 67/67 with usb input; no virtio-input run was part of this list).

## 10. Not run, and deviations from the assignment
- Not run: `tpm`/`tpmcrb` legs (no swtpm); `euroupdate fetch|check` against a channel and a real A/B flip + rollback (4.4); the reference ran no installs beyond the wizard, no entropy series, no q35, no short boots; `chrome-smoke --only localvideo`
  refuses to start without `h264.mp4` (the clip map was without it as asked), so the local video was judged inside the chrome-mem-soaks (3 + 1 sessions, 5 rounds each) instead of one separate run; no virtio-input gui-smoke; no TCG drift.
- The 1 h chrome-mem-soaks of the candidate ran in parallel with other Chromium lanes on separate cores (siblings idle); the reference soak ran beside the drift runs. The key-latency, boot-time and fair runs ran alone.
- TCG matrix: the first candidate run used 512 MiB as in validate-2026-11f (reference run the same), then the user/kernel legs were repeated at 2 GiB (4.2).
- Harness slips, repeated and not counted: the first entropy series (4 x 5 boots) was lost (the `short-boots.sh` copy of `val-runs/scripts` writes no `aslr-N.txt`; redone with the s6 version, `ent2`); the first reference drift runs produced no samples (the 2026.11.17 `uptime` has no vDSO clock; redone with `vdrift-ref.py`);
  the first keylat/boot-time start was stopped by me after one minute (CPU overlap, 4 guests on the boot-time pins) and redone sequentially; one orphaned QEMU of an aborted `[fair]` run was stopped by PID; two update-path attempts (`[upd2]`, `euroupdate fetch`) showed that the CLI is root-only / that the live image boots the installed slot, section 4.4.
- No `.sig` or tracked file changed in the tree (`git status` clean apart from this directory); `toolchain/eupkg/keys/dev.key` was a symlink for the builds and is removed; `userland/*.elf` copied from the main checkout (45).
- Reference worktree `/opt/euroos/wt-11g-ref` (detached at `b30adb6`, for the reference self-test image) was removed after the run.
