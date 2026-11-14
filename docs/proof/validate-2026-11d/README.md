# Validation of `feature/chrome-final` before the release (2026-11-14)

The last two Chromium branches merged, then a full test round on KVM (R630), TCG and the build server. Not pushed.
Bugs: `../../LOADTEST-TRACKER.md` (BUG-190, 191, 192 new; BUG-182 seen again).

| Commit | What |
|---|---|
| `618308f` | merge `feature/chrome-mem2` `7405ea0` (BUG-156 lazy arena, BUG-157 NMI probe, fdpeers, BUG-182 probe): **no conflict** |
| `c892243` | merge `feature/chrome-audio` `fee4cdb` (BUG-181 PCM 1 to 8 channels, `[pcmch]`): **no conflict** (the tracker, `main.rs` and `ring3.rs` merged by themselves; both self-tests run in the boots below) |
| `0fb0472` | merge `feature/app-control` `5a92b1f` (`2e127dd` was already in `chrome-integ`; the newer commit is the 02:00 auto-sync, one line of `.gitignore`) |
| (last) | this proof and the tracker (BUG-190 to 192) |

Worktree `/opt/euroos/wt-final` on .16, branch from `feature/chrome-integ` `fd72e2c`. Images of `0fb0472`, all built with `scripts/build.sh`
(`-j 6`, `nice 10`): self-test image `0ebfe1b859e41e09...`, public image `425076382e7b3966...` (`./scripts/build.sh image`, no options: the
release candidate, kept as `/srv/euroos/final-runs/img/plain.img`), public image with `ui.tour=off;update.check=off` `b040f4be04370f57...`
(the browser runs). "Download" below is the public image 2026.11.13 (`euroos-x86_64-uefi.img.gz`, sha256 of the raw image `1d2f805b37625da1...`).
R630 run directory `/srv/euroos/final-runs`; serial logs and images stay there. The scripts I used are in `drivers/`.

## 1. Verdict

**Klaar voor release: nee.** One finding blocks it, the rest is green or explained.

- **BUG-190 (new, open):** the q35 matrix is 12/13, not 13/13. The `hwprobe` leg types `hwprobe` into the Terminal of the self-test
  image and nothing arrives, in most boots. It passes 5 of 5 on the base `40f853a`, 4 of 6 on the w23 merge, 2 of 6 with the mem merge,
  and 1 of 7 on the final merge. The cause is not found (section 5). It is the self-test image; the public image types fine in
  every test (gui-smoke 67/67 three times, 78/78, 59 of 60 browser starts), but a regression that costs a keyboard is not one to
  ship without an owner's decision.
- Not blocking, but to read: one `[timer-probe] FROZEN` (BUG-182, open since the audio team) ended 1 of 60 browser starts, and one
  of two 2-hour sessions had one step fail on a latency limit while I overloaded the machine (the second session: 179/179).
  `rename-persist` 21/24 is the known BUG-111, unchanged from validate-2026-11c and in the download.

## 2. Results

Compared with the proofs `chrome-integ`, `chrome-mem2`, `chrome-audio`, `validate-2026-11c` and with the download 2026.11.13.

| Test | Result | Comparison |
|---|---|---|
| a. `cargo test` on the host (.16) | 187 binaries, **1 363 passed, 0 failed**, 1 ignored | identical to chrome-integ, mem2, audio |
| b. KVM self-test boots 1, 2, 4, 8 GiB with HD-Audio (`kvm-selftest-boots.txt`) | desktop in all four, 0 crash lines. `[memt]` 7 modes OK, `[pcmch]` 15/15, `[syncsock]` 17/17, `[many-threads]` 320/320, `[churn-forkthreads]` 40/40, `[fdsem]` OK at **all four sizes**. FAILED lines 2/0/0/0: the two at 1 GiB are `GBIG malloc(200 MiB)` (a 1 GiB machine) | markers-compare (`comparison-kvm.txt`): against chrome-mem2 `f08c678` **0 worse** at all four; against chrome-integ `af6f415` **0 worse** at all four; at 1 and 2 GiB `churn-fork` FAIL to OK and the 10 and 9 `fork` FAILED lines (arena allocation) are gone; new: `[pcmch]` OK, `[hda]` OK. `[syncsock]` and `[churn-forkthreads]` run at 1 and 2 GiB now (chrome-integ: SKIP) |
| c1. TCG matrix of 10, restrict=on (`tcg-summary.txt`) | **10/10 desktops, 0 watchdog trips, 0 NMI, 0 fault lines, 0 FROZEN**; `[memt]` 7 modes OK in the four 1 GiB configurations; longest desktop gap 1.7 to 2.0 s | against chrome-integ **0 worse in 10/10**; against chrome-mem2 **0 worse in 10/10** (same R630, same options). Against validate-2026-11c (made on .16 with an open NIC): udrv-net and udrv-crash worse in the 4 `user` configurations, udrv-linux too in the 4 `storage`/`combined` ones: `no DNS reply` and the TCP handshake to an outside address, because the R630 NIC is `restrict=on`. chrome-integ shows the same lines; not a code difference |
| c2. q35 metal matrix, KVM, every NIC restrict=on, slirp bridge of chrome-harness (`q35-matrix.txt`) | **12 of 13 legs PASS** (base, nvme, ahci, e1000e, hda, usb, usbhub, usbnet, usbaudio, scan, printer, power); **`hwprobe` FAIL** | chrome-harness: 13/13 on `40f853a`. BUG-190, section 5 |
| d1. `gui-smoke.py`, public image of the final merge, KVM 4 GiB | **PASS 67/67** | download 2026.11.13: 67/67; markers-compare final against download: **0 worse, 0 changed** (`comparison-public.txt`) |
| d2. `gui-smoke.py --soak 35` (monkey, seed 20261114, usb) | **PASS 78/78**, 2 031 actions, max console latency 350 ms | validate-2026-11c: same seed PASS (300 ms); its virtio run 78/79 |
| d3. `gui-smoke.py --net e1000e` | **PASS 67/67** (KVM) | validate-2026-11c: PASS (TCG) |
| e1. `wizard-install-test.py` | **15/15 checks** (TCG: the install and the second boot) | validate-2026-11c: 15/15 |
| e2. `boot-time.py`, public image, KVM, pin 38-39, 8 boots against 8 of the download, alternating | quiet machine (`boot-time.txt`, second block): **median 2.54 s** (2.50 to 2.73 s; limit 2.8 s) | download: 2.79 s. With 14 other guests running (first block) 2.84 s against 2.93 s of the download. 11c: 2.76 s |
| e3. 100 short boots of the self-test image (`short-boots.txt`) | **107 boots, 0 FAILED lines, 0 crash lines**, `[3d8]` `[3d7]` `[3d3]` `[linux-rand]` OK in all, CSPRNG seeded in all, 428 distinct values of each kind of random number, none repeated | validate-2026-11c: 200 boots, 0 FAILED |
| e4. loadtest soak 125 min, 16 GiB, 4 vCPU (`r630-soak-final-summary.txt`, `r630-soak-compare.txt`) | **3 513 commands ok, 0 errors**, hammer 827 487 requests 0 errors, 0 panic/fault/NMI/FROZEN; kernel heap **+0.23 kB/min** (range 1.3 MiB); tasks, files, kstacks, process pool and free frames flat after 10 min; `[churn-proc]` 2 593 verdicts 0 FAILED; the one `WATCHDOG TRIPPED` at the first `churn proc` is also in the soak of validate-2026-11c | previous soak: heap +0.19 kB/min. `spawn worker` 0.4 s (1.85 s before), `churn fs` 9.7 s (34 s), `fetch` 0.25 s (0.5 s). **Worse:** `churn proc 8` has four rounds of 32 to 34 s and a worst of 44 s (11c: worst 12.6 s), BUG-192. `tasks`/`files` read 334/1 677 against 57/249 in 11c, constant over the run (not a growth; why the level differs I did not check) |
| e5. `rename-persist.py` (installs the public image, three boots, a power cut) | phase 1 renprobe 39/39; **21/24 checks** in 44 runs, the same three failures every time (a Linux-ABI rename does not reach the disk: BUG-111) | identical to validate-2026-11c (21/24) and to the download. The install itself failed in 11 of 55 runs (BUG-191, section 5) |
| f1. Chromium, 20 fresh starts | run 1 **19/20** (start 3: `[timer-probe] FROZEN`, BUG-182), runs 2 and 3 **20/20**; 59 of 60 | chrome-integ and chrome-harness: 20/20 |
| f2. site matrix, 3 rounds in one session | **PASS 36/36**, 0 aborts, 0 refused forks, 0 `[oom]`, 0 FROZEN, 180 live tasks of 512, 0 slots held by ended tasks | chrome-integ 36/36 |
| f3. interaction matrix | **PASS 18/18** | chrome-integ 18/18 |
| f4. local video VP9+Opus with sound (`chrome-localvideo.txt`) | picture advances, currentTime 2.7 to 60.8 s, **12 of 12** five-second windows above the noise floor (RMS 1158), no media error; Opus alone 12/12. H.264+AAC fails as before (5 steps: BUG-161, no such codec in the build) | chrome-integ and chrome-audio: the same |
| f5. Wikimedia 480p VP9 + 6-channel Opus, 2 x 90 s (`chrome-wikimedia-480p.txt`) | **PASS**: picture 22/22 screendumps differ, **sound in 17 of 18 windows** in both rounds | chrome-audio: 17 of 18; chrome-integ before the audio fix: 0 of 18 |
| f6. 2-hour session at 8 GiB, `chrome-mem-soak.py --local-video-dir` (`chrome-soak2h*.txt`) | **B: PASS 179/179**, 11 rounds in 7 397 s, 0 kills, 0 aborts, 0 refused forks, 0 `[oom]`, 0 FROZEN, clip with sound 12/12 in every round. **A: 178/179**, 11 rounds in 7 468 s, no kill; the one failed step is the Wikipedia page in round 3: the page loaded, but the slowest console answer was 1 243 ms (limit 1 000 ms) while I had started twenty guests on the R630 | chrome-mem2 at 8 GiB: 179/0 steps, in use +4.3 MiB/h. Here +10.8 and +11.7 MiB/h (named RAM files and `big` grow +9 and +10 MiB/h); mem-curve prints "NOT flat" in both because one census saw 70 and 94 MiB without an owner (a fork in progress; the same remark as chrome-mem2 p4); lowest free demand pool 2 335 of 3 232 MiB |

## 3. Differences against the proofs, explained

- `GBIG malloc(200 MiB) FAILED` at 1 GiB: a 1 GiB machine; chrome-mem2 has the same.
- udrv-net, udrv-crash, udrv-linux, conn: see c1 (NIC `restrict=on`).
- Boot time 2.84 s in the loaded block: the machine was running a soak, a 2-hour browser session and ten guests; the download, booted
  beside it, was 2.93 s.
- The WATCHDOG line in the soak: also in validate-2026-11c (one line, at the first `churn proc 8`).
- The one failed Wikipedia step in session A: a latency limit crossed while I loaded the machine; the same step passed in the other 10 rounds of session A and in all 11 of session B.

## 4. What I did on top of the list

- The FROZEN of run 1 asked for more starts: two more runs of 20 (40/40).
- The first 2-hour session was started with a clip directory that held the H.264 clip too (its 5 steps fail by design) and was stopped after 4 rounds
  (1 hour); it was restarted with `vp9.webm` only, as chrome-mem2 did. Session A is that restart, session B a second one on a quieter machine.
- `hwprobe` was repeated and bisected with extra builds: self-test images of `fd72e2c`, `7405ea0`, `fee4cdb`, `7d6d073`, `3990f17` and the public
  images of `fd72e2c` and `7405ea0` (built in this worktree by detaching HEAD; the branch is back at the merge).

## 5. The three open findings

**BUG-190, the `hwprobe` leg.** The matrix types `hwprobe` through QMP 10 s after the desktop loop starts. In the failing boots the xHCI
reports arrive (`[krep]`, all eight codes) but the Terminal never shows the text, not even for four commands typed 10 to 106 s later
(`screenshots/hwprobe-before-typing.png`, `hwprobe-after-4-commands-106s.png`), while the clock keeps running. Not a layout problem (the same with an
unmapped table), not a timing problem (repeated waits), not the NIC. Pass/fail of the leg alone (KVM, restrict=on, self-test image): `40f853a` 5/0,
`7d6d073` 4/2, `3990f17` 2/4, `fd72e2c` 3/2, `7405ea0` 0/1, `fee4cdb` 0/1, final 1/7 (`q35-matrix.txt`). It starts with the w23 merge and gets
worse with the mem merge. A guess, not tested: the hosted GTK demo window of the self-test image takes the keyboard focus in the failing boots (BUG-112: it takes no input).

**BUG-191, the install wizard.** `rename-persist.py` phase 0 sometimes ends with `Installation FAILED`: `EuroFs::format` returns an error that
`instexec.rs` throws away (`Err(_) => 0`), 0 steps are provisioned. It follows the load of the host and is in the download too: ten installs at once, download 6 of
10 failed and the final merge 2 of 10; with little else running 0 of 7 final installs, 0 of 3 download, 0 of 3 `40f853a`, 0 of 6 chrome-integ, 0 of 3 chrome-mem2
(`rename-persist.txt`). Not a regression of this merge; not fixed.

**BUG-192, `churn proc 8` in the soak.** Four rounds of 32 to 34 s (t = 1 359, 2 986, 4 717, 6 360 s, every 27 minutes), a worst of 44 s; 11c: 12.6 s.
The desktop loop is away inside "devices and windows" for those 32 s, no tick is lost, other commands are normal. Cause not found.

## 6. Deviations

- `userland/{doom,fbtest,ipcrecv,ipcsend,isotest,mcat,mdns,mecho,menv,mmutex,mpthread,msock,msum,mthread,mtrack,mupper,muslfile,muslreal,mwrite,randprobe,tlscount,worker}.elf`
  copied from the main checkout (no musl-gcc on .16), as the other worktrees have; they are not tracked.
- The R630 was heavily loaded at times (up to about 25 guests: my own, all pinned); results that depend on it are marked in the table.
- gui-smoke ran with its default network (`net=none`), not `restrict`.
- 107 short boots, not 100, in six lanes of 2 vCPU (the lanes were stopped at 101 by a stop file; some were mid-boot).
- The TCG comparison against validate-2026-11c is not like for like (open NIC there), see c1; against chrome-integ and chrome-mem2 it is.
- There is no ssh path between the servers: images and proof text were streamed through the laptop without being stored there.
- `dev.key` was linked in the worktree for the builds only and is removed again; no `.sig` file or `dev.pub` is in the commit.
