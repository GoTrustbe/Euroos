# BUG-190 and BUG-191 on `feature/chrome-final` (2026-11-14)

Branch `feature/chrome-fix190` from `feature/chrome-final` `ac36955` (worktree `/opt/euroos/wt-fix190` on .16). Not pushed.
R630 run directory `/srv/euroos/fix190-runs` (CPUs 0-39); serial logs and images stay there. The scripts I used are in `drivers/`.
Tracker: `../../LOADTEST-TRACKER.md` (BUG-190 and BUG-191 fixed, BUG-192 analysed, new BUG-200 to BUG-202).

| Commit | What |
|---|---|
| `946989b` | BUG-190, the test first: `[keyfocus]` names the window the keyboard goes to on the first desktop frame |
| `e909c2d` | BUG-191, logging first: the error of `EuroFs::format`, refused writes and flushes, block requests that time out |
| `f178250` | BUG-190 fix: the empty GTK frame opens only with the demo that draws in it; staged demo input nobody read is dropped |
| `a38230d` | BUG-191 fix: a block request may take 60 s, a timed-out request is waited for before its buffers are reused, the install reports why it failed |
| (last) | this proof and the tracker |

Images of `a38230d`, built with `scripts/build.sh` (`-j 6`, `nice 10`): self-test `7ccc756aca1c6643...`, public `50c3e20d7b15b586...`
(no options), public with `ui.tour=off;update.check=off` `3f8a5266aec5c4e1...` (browser runs). The self-test image of `ac36955`
used for the comparisons is the validator's (`0ebfe1b859e41e09...`).

## 1. BUG-190: typed text does not reach the Terminal (q35 `hwprobe` leg)

**Cause.** Keys go to the topmost visible window. In the self-test image the hosted X frame (window 2, `EuroOS GTK  -  ggtk`) is
visible from the first frame and above the Terminal in the z-order, but the live GTK demo that would draw in it only starts with
`--features live-demos`. The frame has no client, so the character loop of the desktop took every key and dropped it. The boots
that passed did so by accident: the `gxlive` self-test stages three keys and two left clicks for its X client; when no client read
them, the click (screen centre, inside the Terminal) reached the desktop later and raised the Terminal. When the wait loop of the
`gsdl` run ate that input first (the six `[sc]` lines in its log), nothing raised the Terminal. The W23 and mem merges changed that
timing. The validator's guess was right about the window, not about the reason: the ggtk demo (BUG-112) does not run in this image.

**How it was found** (`hwprobe-runs.txt`, `kroute-diagnostic.txt`): a diagnostic build logged which code took each scancode after
the desktop started and the focus of every character in the desktop loop. Failing boots: `char 'h' in the desktop loop: focus
Some("EuroOS GTK  -  ggtk") term_focused=false`; passing boots: `focus Some("Terminal  -  /bin/sh")`. The key path itself
(xHCI report, `inject_keyboard`, scancode ring, `poll_key` at `main.rs:6205`) delivered every key in both. In 20 boots (12 of
`ac36955`, 8 of the diagnostic build; 9 passed, 11 failed) the `[sc]` lines predicted the result every time.

| hwprobe leg alone, KVM, restrict=on | boots | PASS | FAIL | `[keyfocus]` |
|---|---|---|---|---|
| `ac36955` (validator's image) | 12 | 4 | 8 | (not in the image) |
| `ac36955` + diagnostics | 8 | 5 | 3 | - |
| `946989b` + `e909c2d` (the test, no fix) | 4 | 0 | 4 | **FAILED 4/4** |
| `f178250` (fix) | 24 | **24** | 0 | OK 24/24; 17 of them with the `[sc]` lines that failed every time before |
| `a38230d` (final image) | 20 | **20** | 0 | OK 20/20 |

Six lanes of four CPUs ran side by side, the machine was loaded (the validator saw more failures under load). The fix also logs the
staged input it drops: in 7 of the 24 fix boots the six scancodes and the left click were still there.

## 2. BUG-191: "Installation FAILED" under host load (in the download too)

**The error, logged first** (`installs-side-by-side.txt`). Ten installs side by side on eight host CPUs (phase 0 of
`rename-persist.py`, `drivers/inst.py`), public image with the logging of `e909c2d`: **10 of 10 failed**, every one the same way:
`[blk-diag] request type 4 sector 0 TIMED OUT after 777..1697 ms`, `EuroFs::format on virtio disk 0 failed: IoError`, then the
next flush timed out too (one request still in the device). Type 4 is `VIRTIO_BLK_T_FLUSH`: QEMU fsyncs the image file, and with
nine other guests writing that takes seconds. `kick_and_wait` gave a request 40 M spins (about a second) and then returned false
while the request stayed in the device; the next request reused its descriptors and buffers. `instexec` threw the format error away
and ignored the result of every boot-disk write and of the flush.

**Fix** (`a38230d`): a request may take 60 s, measured on the TSC; slower than 2 s is logged; a request that did time out is
waited for before anything of it is reused, and a disk that still holds one refuses the next request instead of mixing two. The
install checks every write and the flush and fails with the first reason in words; `cache_flush` reports a refused write.

| Ten installs side by side, eight CPUs | OK | FAILED |
|---|---|---|
| public image with logging, no fix | 0 | 10 |
| `a38230d`, first batch | **10** | 0 |
| `a38230d`, final public image | **10** | 0 |

The flushes that failed before took **3.2 to 11.6 s** with the fix (logged as `[blk] slow flush`). `wizard-install-test.py`:
**15/15** (`wizard-install-test.txt`).

**A clear message.** A blank target that refuses writes (virtio `readonly=on`): the final merge of validate-2026-11d says
`Installation FAILED (see the serial log)`, and its remount check even reported `hostname=true user=true euroid-store=true`
(it reads the write-back cache, BUG-200). `a38230d`: `[wizard] Installation FAILED: the disk refused 40867 write(s) of the boot
files (first at sector 0)`; the Failed page of the wizard lists that line.

## 3. Final checks of `a38230d`

| Test | Result | Against validate-2026-11d |
|---|---|---|
| `cargo test` on .16 (`cargo-test.txt`) | 187 binaries, **1 363 passed, 0 failed**, 1 ignored | identical |
| KVM self-test boots 1, 2, 4, 8 GiB, HD-Audio (`kvm-selftest-boots.txt`) | desktop in all four, 0 crash lines, `[keyfocus] OK`; FAILED lines 2/0/0/0 (the two `GBIG malloc(200 MiB)` at 1 GiB) | markers-compare **0 worse** at all four sizes (also against `ac36955` booted beside it) |
| TCG matrix of 10, restrict=on (`tcg-summary.txt`) | **10/10 desktops**, 0 watchdog, 0 NMI, 0 fault lines, 0 FROZEN, `[keyfocus] OK` in 10/10 | markers-compare against `validate-2026-11d/tcg-*.markers`: **0 worse in 8**, in 2 (`storage`/`combined-smp1`) `bgnet-cap` FAIL; rerun of both: OK, as on `ac36955` beside it (section 4) |
| q35 metal matrix, KVM, restrict=on (`q35-matrix.txt`) | **13/13** | validate-2026-11d 12/13 (hwprobe) |
| `gui-smoke.py`, public image (`gui-smoke-public.txt`) | **PASS 67/67** | 67/67; markers-compare **0 worse**, only new: `keyfocus` |
| Chromium, 20 fresh starts (`chrome-starts20.txt`) | **20/20**, 0 FROZEN | 59 of 60 |
| site matrix, 3 rounds (`chrome-site-matrix-sites3.txt`) | **PASS 36/36**, 0 aborts, 0 FROZEN, 0 NMI probes (second session; the first: 35/36, section 4) | 36/36 |
| installs and wizard | see section 2 | |

## 4. Deviations and what was not done

- **KVM boots, first set:** started together with about 30 other guests, `[fuzz] kernel-survived=false` at 1, 2 and 4 GiB: gfuzz
  ran into its run deadline (late ticks, lost ticks). Booted again at a quieter moment, beside the `ac36955` image: fuzz OK in all 8
  boots. The fuzzer runs long before any changed code. The table uses the second set; both are in `kvm-selftest-boots.txt`.
- **TCG `bgnet-cap`:** in the first matrix (while a 16 GiB soak, the site matrix and the KVM boots ran) `msock` as a background
  process had not run yet when it was checked (`refused 0 time(s), still running`) in the two smp1 storage configurations. Rerun of
  both, with `ac36955` beside it: OK in all four.
- **Site matrix, first session (`chrome-site-matrix-sites3-a.txt`, `bug202-serial.txt`): 35/36.** The harness saw a 13 s console
  answer (the machine ran the soak, the TCG matrix and the KVM boots), sent an NMI, and the NMI probe ended the serial TX task as a
  "kernel stack overflow" (BUG-202, not related to these changes). The step `site euro-os` failed on that line. A second session
  on the same image: 36/36.
- **BUG-192** (`soak-bug192.txt`): analysed, not fixed; see the tracker. A 55-minute soak of the final self-test image had 34, 38
  and 42 s rounds already in the first half hour. In the 42 s round the desktop loop (which runs `churn proc`) was `Ready` for
  3 979 ticks and ran about 100, in `memset` and `virtio_blk::kick_and_wait` under `proc_churn_n`; five other tasks took 647 ticks
  each. The eight processes wait for the CPU, not for a lock. The 27-minute period was not seen. The soak itself ended clean (0 errors, hammer 377 827 requests 0 errors).
- **BUG-182** (`[timer-probe] FROZEN`): not seen in 20 starts and the site sessions; no new data, not worked on.
- New, open: BUG-200 (the install check after the remount reads the write-back cache on virtio disk 0), BUG-201 (the wizard offers
  a read-only disk), BUG-202 (the NMI probe scans past the top of a small kernel stack and the interrupted task is ended as a stack
  overflow; it ended the serial TX task in the first site session).
- 1 vCPU and 512 MiB for the hwprobe leg, as the matrix script sets it. `scripts/` was not changed.
- `userland/*.elf` that git ignores were copied from `/opt/euroos/wt-final`. `dev.key` was a symlink for the builds only and is
  removed; no `.sig` file or `dev.pub` is in a commit.
- No ssh path between the servers: images and text were streamed through the laptop without being stored there.
