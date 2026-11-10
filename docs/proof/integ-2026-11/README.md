# Integration of bug-gpf, fast-boot, robust, settings and help into app-control (2026-11-10)

Branch `feature/integ-2026-11` on `feature/app-control` `a36ef4b` (the head had
not moved: the nightly sync added nothing since the five branches started).
Not merged into `feature/app-control`, not pushed. Bugs: `../../LOADTEST-TRACKER.md`.

| Commit | What |
|---|---|
| `3ee1b0f` | merge `feature/bug-gpf` (`83930f3` … `1bb04c1`): BUG-037, BUG-039, BUG-040, BUG-050, euromm hardening |
| `9ebc65e` | merge `feature/fast-boot` (`a03adcf` … `10ff36c`): `SELFTEST` switch, `[boot-time]`, `[net-up]`, NVMe format fix |
| `11c30d2` | merge `feature/robust` (`43b59b3` … `16c9640`): panics become errors, `try_build_address_space*`, bounded reads, high BARs, `gui-smoke.py` |
| `05218c2` | merge `feature/settings` (`25768fb` … `de95619`): `europrefs`, `prefs.rs`, form toolkit, the Settings panel |
| `bc198f3` | merge `feature/help` (`476f059` … `bad463e`): help panel, shortcut overview, tour, tooltips, Esc, F1/F2 |
| `fbf4f8b` | tracker: one number per bug (fast-boot: BUG-046, BUG-047, BUG-048) |
| `c5f1665` | the settings, help and robust self-tests called from `main.rs` under `SELFTEST` |
| `b65b395` | BUG-045: a dragged window stops at the dock |
| `3da84bf` | BUG-044: Alt+Tab and Alt+1..4 in `poll_key_ex` |
| `961244b` | tour: `tour.done` in the desktop preferences; the harnesses handle the tour |
| `aa14488` | the old inline `help` branch of the Terminal dispatch removed |
| `e31c753` | `populate_fs`: fifteen `unwrap`s become log lines |
| `a19d507` | `ring3.rs`: the ten callers of `build_address_space*` use the `try_` variants |
| `36af890` | help texts follow the merged code |
| `7bf0a2d` | BUG-051: the periodic integrity check writes its log line in system context |
| `182b19c` | BUG-047: the inline bring-up saves its address before the lookups |
| `fb18477` | BUG-059: the `[robust-fs]` self-test makes its own `/tmp` (self-test code only) |
| `a42b265` | `gui-smoke.py`: the monkey copes with an open notification list and an open context menu |
| (last) | this proof, the tracker, `BOOT-TIME.md`, `GUI-TESTING.md` |

Which image was tested:

- **Release image**: built at `182b19c` (sha256 `4d3f94e7…`). The two code
  commits after it do not change it: `fb18477` touches a function compiled
  only into the self-test image, `a42b265` a harness script.
- **Self-test image**: every test ran on `182b19c` (sha256 `c58f097e…`) and,
  after BUG-059 was found in the boot of the soak guest, again on `fb18477`
  (sha256 `23c0be2a…`). The tables give the `fb18477` round; where the first
  round is meant it says so.
- A round before that, on `7bf0a2d`, was stopped when BUG-047 turned out to
  be small enough to fix; what it had finished gave the same verdicts (boot
  time 2.69 s and 2.21 s).

Serial logs stay on the build servers (`*.log` is not committed); the
`.markers` files hold the verdict lines of each log plus its `[lk]`, `[fork]`
and `[boot-time]` lines.

## Conflicts

| File | Conflict | Resolution |
|---|---|---|
| `kernel/src/main.rs` (fast-boot) | two hunks: fast-boot had indented the self-test blocks under `if SELFTEST {`, bug-gpf had added `ring3::spawn_window_selftest` (`[bug040]`) and `smp::ap_tss_selftest` (`[ap-tss]`) inside them | fast-boot's blocks, with the two calls at their places inside the `SELFTEST` blocks |
| `kernel/src/main.rs` (settings) | the `// T2-hook` self-test line sat in a block fast-boot had moved under `SELFTEST` | fast-boot's block; `prefs::selftest` and `settings_ui::state_selftest` after `session::quota_selftest`, inside it. The other nine T2 hooks merged by themselves and were read at their places |
| `kernel/src/main.rs` (help) | the `// T3-hook` `help::selftest()` line, same cause | fast-boot's block; the call after `tooltip::selftest()`, inside it. The other eight T3 hooks merged by themselves |
| `docs/LOADTEST-TRACKER.md` | no textual conflict, but BUG-041 to BUG-043 twice, a stale "BUG-040 reserved" line, BUG-050 before BUG-041 | `fbf4f8b`: robust keeps 041 to 045; fast-boot's NVMe bug is BUG-046, ARP before `net::save` BUG-047, self-tests in the release image BUG-048; the bug-gpf row of the run table said 041 for BUG-050; sections in numeric order; `proof/fast-boot-2026-11/README.md` and `BOOT-TIME.md` follow (commit messages keep the old numbers) |

`smp.rs` (bug-gpf and fast-boot), `xhci.rs` (robust and help's F1/F2 row),
`shell.rs`, `ps2.rs`, `compositor.rs` and the other documents merged without a
conflict marker. Checked instead of trusted: `[ap-tss]` OK in every boot with
more than one core; F1, F2 and Alt+Tab from a USB keyboard (`usb-f1-check.txt`,
and the Alt-Tab steps of gui-smoke over `usb-kbd`); `[help]`, `[prefs-*]`,
`[uikit]`, `[set-*]`, `[robust*]` OK in all ten matrix boots.

## The connecting work

- **a. Self-tests** (`c5f1665`). `[robust]` and `[robust-fs]` were called from
  inside `shell::selftest` and `textedit::selftest`; they are public and
  `main.rs` calls them next to those two, in the `SELFTEST` blocks, as it does
  `prefs::selftest`, `settings_ui::state_selftest` and `help::selftest`. The
  release image prints none of their lines: its verdict lines are the 25 tags
  fast-boot kept (`release-image-verdict-tags.txt`).
- **b. BUG-045** (`b65b395`). `x` is clamped to `SIDEBAR_W` in the drag and in
  the un-snap on pickup. `bug045-after.txt`: the drag of robust's repro script
  leaves the window at the dock's right edge.
- **c. BUG-044** (`3da84bf`). The two arms of `poll_key` in `poll_key_ex`.
  `KNOWN_ISSUES` is empty; the step "Alt-Tab with the text editor in front"
  passes under TCG and KVM, USB and virtio keyboard.
- **d. Tour** (`961244b`). `tour.done` in the user's `desktop.conf` through
  `prefs::open_user`. On the installed system: "Do not show again" in boot 1,
  `desktop.conf (4 key(s))` and no tour in boot 2 (`persistence-installed.txt`).
  `gui-smoke.py`, `wizard-install-test.py` and `screenshot.py` press Esc when
  the serial log says the tour is up; `boot-time.py` writes `ui.tour=off` into
  its private image copy unless `--tour keep`.
- **e.** (`aa14488`) The eleven-line command list behind `exec_cmd == "help"`
  could not be reached after `help::shell_line`; removed.
- **f.** (`e31c753`) `populate_fs`: a refused `create_dir` or `write_file` is a
  `[euro] populate: …` line and a count at the end.
- **g. Help texts** (`36af890`). EuroBeheer rewritten for the nine sections,
  its keys and what is saved where; "Display settings" opens Display, not
  System; the note that Alt+Tab does not work in editors replaced by what is
  left (programs that drain raw scancodes); EuroText: files over 8 MiB or a
  quarter of the free heap are refused; EuroPaint: fewer undo steps when memory
  is short. EuroWeb, EuroFiles and `tooltip::paint_tip` were read against
  `webview.rs`, `files.rs` and the toolbar constants of `paint.rs`: nothing to
  change.
- **h. `ring3.rs`** (`a19d507`). Ten callers on the `try_` builders, each
  giving back what it took (table in the commit message); `run_args` also lost
  its `expect("fg-arena")`. Not provoked with zero free frames at a process
  start (the builders themselves are, by `[robust-paging]`); judged by the
  churn tests and the soak: `[churn-*]` verdicts unchanged in the matrix, the
  soak below. Left: BUG-053.
- **i. "Checks since start: 2" and an empty log** (`7bf0a2d`, BUG-051). Both
  were true: the passes ran, their log line was dropped because the periodic
  pass runs under the signed-in user. `scrub-log-before-after.txt`,
  `screenshots/11-eurobeheer-storage-last-result.png`.
- **BUG-047** (`182b19c`). Half fixed by BUG-039 (ARP, ping, DNS); the HTTP and
  HTTPS probes of the inline bring-up still timed out because TCP segments are
  sorted by the saved address. The address is saved before the lookups now.
  `kernel-smp4` under TCG: `phase net` 11.8 s (fast-boot, lookups failing
  early), 22.2 s (after the merge, probes timing out), 16.0 s (fixed, HTTP and
  HTTPS answered).
- **BUG-059** (`fb18477`), found in the boot of the soak guest: `[robust-fs]`
  FAILED on a root the installer self-test had provisioned on the blank disk,
  because the test wrote into a `/tmp` that did not exist yet. It creates the
  directory now; OK in the second soak boot and in all ten matrix boots.

## Build server (QEMU 10.2, TCG)

`tcg-boot.sh` and `tcg-matrix.sh` as in `../fast-boot-2026-11/` (two queues,
each QEMU stopped by its own PID, NMI probe on a silent boot: none needed),
self-test image, `-nic user,model=e1000e`, 120 s after the desktop.

| Runs | Result |
|---|---|
| Host tests, `cargo test` (default members) | 186 suites, 1 304 passed, 0 failed, 1 ignored (`hosttests.txt`) |
| `user-smp{4,1}-{iommu,noiommu}`, `kernel-smp{4,1}`, `storage-smp{4,1}-iommu-1g`, `combined-smp{4,1}-iommu-1g` | 10 of 10 to the desktop, no fault line. 27 comparisons against `../bug-gpf-2026-11/`, `../fast-boot-2026-11/`, `../robust-2026-11/`, `../settings-2026-11/`, `../help-2026-11/` (`comparison-tcg.txt`): **0 worse**. New against bug-gpf: the 14 tags of robust, settings and help; in the kernel-driver boots also `[net] HTTP GET … ✓` and `HTTPS GET … ✓` (BUG-047) |
| `[lk]` | `boot-total order inversions=0 … task-lock-with-IF0=0` in all ten (`tcg-summary.txt`) |
| `megaraid-proof.sh` with IOMMU, and with `NO_IOMMU=1` (4 boots) | 0 markers worse than bug-gpf (`comparison-mr.txt`); host checks 4/4 in both |
| modern virtio only (`tcg-virtio-boot.sh`, `MODE=modern`, 1 GiB, smp 2) | desktop; 0 worse than `../bug-gpf-2026-11/tcg-dev-modern.markers` (`comparison-tcg-virtio.txt`) |
| CI q35 matrix, `run-metal-matrix.py --legs base,nvme,ahci,e1000e,hda,usb,usbhub,usbaudio,usbnet,scan,printer,power,hwprobe` | 13/13 PASS (`q35-matrix.txt`) |

The first round, on `182b19c`, gave the same verdicts in every row (27 + 4 + 1
comparisons 0 worse, q35 13/13, 186 host suites).

Against fast-boot the base of `storage-smp4-iommu-1g` is its complete rerun
`-r1`; its first boot of that configuration hung half way (BUG-037, fixed by
bug-gpf) and has 150 markers fewer.

### Slower, not worse: the self-test image under TCG (BUG-056)

No marker changed, but the ten boots reach the desktop 144 to 159 s after
kernel entry at 512 MiB (`tcg-summary.txt`) where fast-boot had 97 to 114 s. `[boot-time]`:
`subsystems` 3 to 9 s more (the new self-tests) and `lockscreen` 94 s instead
of 50 s in every configuration. Bisected over the merge commits and then with
throwaway TSC marks (`lockscreen-bisect.txt`): the sign-ins take milliseconds;
the time is the lock screen's background, `fill_rounded_rect_grad` over the
whole 1920x1080 screen, 24 s per frame up to the robust merge and 40 s since
the settings merge, with no change in that function (the kernel has no
hardware float: every `f32` operation is a soft-float call). The release image
is not slow there: live medium, `lockscreen` 9 ms (skipped); installed system
under KVM, first frame plus a refused sign-in 0.44 s. Left open.

### Release image (`./scripts/build.sh image`), TCG

| Test | Result |
|---|---|
| `wizard-install-test.py` | 14/14 (`wizard-install-test.txt`); the tour closed with Esc in both runs |
| `gui-smoke.py` (usb-kbd + usb-tablet, smp 2, 2 GiB) | PASS, 56/56 steps (`gui-smoke-tcg.txt`): robust's 55 with BUG-044 counting now, plus the tour step |
| Settings survive a restart: installed disk, settings changed in the panel, QEMU quit, second boot (`persist-a.txt`, `persist-b.txt`, harness `../settings-2026-11/t2-gui.py`) | boot 2 reads back keyboard `de-qwertz`, time zone Europe/Brussels, 1 domain + 1 app rule, 12-hour clock, wallpaper mist, text 110 %, `tour.done`: 5/5 expectations, no tour (`persistence-installed.txt`) |
| Network on open user-net: `net`, `ping 10.0.2.2`, `nslookup`, `fetch`, `https example.com` | lease 10 ms after `[net-up]` started; ping OK, DNS answer, HTTP 200 (889 bytes), TLS 1.3 HTTP 200 (`network-open.txt`) |
| Boot, 2 vCPU, 2 GiB (3 boots) | desktop 5.9 to 6.7 s after QEMU start (fast-boot: 6.5 s) |
| No self-test of the integration in the release image | 0 lines of `[robust*]`, `[prefs-*]`, `[uikit]`, `[help]` |

## Dell R630 (KVM)

Only the two built images and five harness scripts (`boot-time.py`,
`gui-smoke.py`, `loadtest.py`, `loadtest-http-peer.py`, `markers-compare.py`)
were copied there, to `/srv/euroos/integ-runs`; no source tree, no key. Every
QEMU with SLIRP `restrict=on` or without a NIC, on its own CPUs, no VFIO.

### Boot time of the release image (`kvm-5x.txt`)

`boot-time.py`, `-smp 2 -m 2G`, `restrict=on`, pinned to two cores, host time
from QEMU start to `[desktop] interactive loop started`:

| Configuration | fast-boot (`3ab5d2c`), median (range) | integrated (`182b19c`), median (range) |
|---|---|---|
| SATA boot medium + legacy virtio-net | 2.95 s (2.88 to 3.06) | **2.71 s** (2.42 to 2.92) |
| modern virtio only | 3.31 s (2.75 to 4.23); repeat 2.70 s (2.53 to 2.77) | **2.34 s** (2.05 to 2.43) |

Kernel entry to desktop 812 ms (`[boot-time]`, boot 3; fast-boot 1.0 s). The
tour costs one read of `desktop.conf` in the first loop iteration, after the
mark.

### GUI: smoke pass and monkey soak, 35 min each

`gui-smoke.py --kvm --soak 35` on the release image. **With the harness as
merged, two of four USB runs were not green; the desktop was not at fault in
either, and with the harness fixed (`a42b265`) every run passes.** All eight:

| Run | Harness | Smoke pass | Monkey, 35 min | Functional checks | Verdict |
|---|---|---|---|---|---|
| USB, smp 4, 4 GiB, no NIC, seed 20261110 (`gui-soak-usb.txt`) | final | 56/56 | 2 217 actions | 11/11 | **PASS**, console max 250 ms |
| USB, same, seed 20261114 (`gui-soak-usb-b.txt`) | final | 56/56 | 1 958 | 9/9 | **PASS**, 200 ms |
| virtio, smp 2, 2 GiB, `restrict=on`, seed 20261111 (`gui-soak-virtio.txt`) | final | 56/56 | 2 190 | 10/10 | **PASS**, 2 005 ms |
| USB, seed 20261110 (`gui-soak-usb-run1.txt`) | as merged | 56/56 | 2 228 | 10/11 | FAIL: the check after 2 200 actions |
| USB, seed 20261110 (`gui-soak-usb-run2.txt`) | as merged | 56/56 | stopped at 1 750 | 8/8 | "CRASH/HANG: screen frozen" |
| USB, seed 20261112 (`-run3`) | as merged | 56/56 | 2 182 | 10/10 | PASS |
| USB, seed 20261113 (`-run4`) | as merged | 56/56 | 2 155 | 10/10 | PASS |
| virtio, seed 20261111 (`gui-soak-virtio-run1.txt`) | as merged | 56/56 | 2 249 | 11/11 | PASS |

No panic or fault line in any of the eight serial logs
(`gui-soak-serial-summary.txt`). What the two red runs were:

- **Run 2, "screen frozen".** The harness moved the pointer, looked once
  0.3 s later, and called two such checkpoints in a row a frozen screen. The
  notification list was open (`screenshots/gui-soak-usb-run2-frozen-verdict.png`):
  it closes only by a click on the status panel, and while it is open the
  desktop repaints everything in every loop iteration. Measured on the R630
  with seven windows (`frame-time.txt`): a pointer move shows after 0.3 to
  0.4 s instead of 0.04 s, a console echo takes 150 ms instead of 50 ms; the
  release image of `11c30d2`, before the settings and help merges, gives the
  same figures. Between the two looks the loop ran a typed command and
  answered the console; the NMI probe the harness sent found it painting
  (`draw_window_body` -> `fill_rounded_rect` -> soft-float compare). BUG-057,
  open; the harness now waits up to 5 s.
- **Run 1, one failed functional check.** The monkey had opened a context
  menu just before the check. Esc does not close a menu and the next left
  click belongs to it; with no window button on screen to click first, that
  click was the one on the EU mark: no launcher, and the typed command went
  elsewhere. The harness now clicks beside the dock first.

Also tried while looking for a real hang: every dock app dragged to the bottom
edge (`drag-to-bottom.txt`, 11 of 11 alive), and the calculator resized to the
smallest height the desktop allows, which does not hang but paints its keypad
down the screen (BUG-058, `screenshots/bug058-calculator-200px.png`).

### Soak

`loadtest.py --kvm --smp 4 --nic e1000e --restrict --nmi-on-hang --mem 16G
--disks 8G --host-http --hostfwd 18480 --hammer 4 --procs 32 --scenario soak
--soak-churn-proc 8 --duration 7500`, self-test image, `net.driver=user`: the
configuration of `../bug-gpf-2026-11/` (`r630-soak-a`), `churn proc 8` (nine
glibc launches) in every round, four httpd clients the whole time. Run twice,
because the first one's boot showed BUG-059:

| | `182b19c` (`r630-soak-182b19c*`) | `fb18477`, the head (`r630-soak*`) |
|---|---|---|
| commands | 2 868, all ok, `completed clean` | 2 899, all ok, `completed clean` |
| `churn proc` commands / glibc launches (`GLIBC-DYNAMIC-OK`) | 367 / 2 955, every `[churn-proc]` verdict OK | 371 / 2 987, every `[churn-proc]` verdict OK |
| `heap_used` first / at 10 min / last | 148 238 928 / 148 267 424 / 148 292 256 | 148 219 472 / 148 285 344 / 148 277 664 |
| slope after 10 min (least squares) | **+0.24 kB/min** (range 105 kB) | **+0.25 kB/min** (range 105 kB) |
| tasks / files / kernel stacks / process pool | 56 / 186 / 0 / 887 296, flat | 56 / 187 / 0 / 887 296, flat |
| free frames | 2 964 768 -> 2 981 408 (the first wave's zombies reaped) | 2 964 767 -> 2 981 407 |
| httpd during the soak | 948 639 answered, 11 failed, p50 4.8 ms, p99 123 ms | 1 006 005 answered, 11 failed, p50 4.5 ms, p99 122 ms |
| serial | 0 panics, 0 fault lines, 0 watchdog lines, 0 refused frame frees, 0 "page tables" refusals | the same: 0 of each |
| boot of the guest against bug-gpf's soak boot (`comparison-r630-soak-boot.txt`) | 0 worse; `[robust-fs]` FAILED (BUG-059) | 0 worse; `[robust-fs]` OK |

bug-gpf alone: +0.23 kB/min, 896 756 answered / 12 failed, p99 123 ms. The
`ring3.rs` change (`a19d507`) is in both runs: no churn verdict worse, no
refusal taken (the guest never ran out of frames), so it stays.

The failed lines in the guest's boot that are not new: `[udrv-net]`,
`[udrv-crash]`, `[udrv-linux]`, `[conn]` (no route out with `restrict=on`) and
`[fuzz]`, the same five as in bug-gpf's soak boot.

## Screenshots (release image, 1920x1080, QEMU `screendump`)

All were looked at. `live-shots.txt` and `storage-shots.txt` are the input
scripts (`t2-gui.py`).

| File | Shows |
|---|---|
| `01-tour-step1.png` | first desktop, tour card 1 of 6 |
| `02-desktop.png` | the desktop after Esc |
| `03-help-terminal.png` | F1 with the Terminal in front |
| `04-eurobeheer-system.png`, `06-eurobeheer-display.png`, `07-eurobeheer-storage.png`, `08-eurobeheer-updates.png` | EuroBeheer: four of the nine sections |
| `05-help-eurobeheer.png` | the rewritten EuroBeheer help page |
| `09-shortcuts.png` | F2, with the new note on Alt+Tab |
| `10-eurobeheer-storage-top.png`, `11-eurobeheer-storage-last-result.png` | Storage after two periodic checks: "Checks since start 2" and the last line of `/var/log/fsck.log` (BUG-051) |
| `bug045-window-stops-at-dock.png` | the Terminal after the drag of robust's repro script |
| `installed-boot1-tour.png`, `installed-boot2-desktop.png`, `installed-boot2-display.png` | the installed system: tour in boot 1; boot 2 without it, with the saved background, clock and text size |
| `wizard-1-welcome.png`, `wizard-9-installed-desktop.png`, `gui-smoke-tour-step-1.png` | the harnesses with the tour closed, and the frame gui-smoke saw before it closed it |
| `kvm-tour-step1.png`, `kvm-desktop.png` | the same two frames as `01` and `02` on the R630 (`boot-time.py --tour keep`, and with its default `ui.tour=off`) |
| `gui-soak-usb-final.png`, `gui-soak-virtio-final.png`, `gui-soak-usb-run1-final.png`, `gui-soak-usb-run2-frozen-verdict.png` | the desktops after 35 min of monkey; the last two with the notification list open (BUG-057) |
| `bug058-calculator-200px.png` | the calculator at the smallest window height |

Seen in them and written down, not changed: the seam in the default background
at half the screen width (BUG-054; it is in fast-boot's screenshots too); on
Storage "1 MiB of 8 MiB used (21%)" (the figures are rounded down, the
percentage is not) and "no virtio-blk devices" for a system booted from SATA
(BUG-055).

## Open

- BUG-057: the notification list stays open and makes the desktop sluggish;
  not from this integration (same figures on `11c30d2`), not changed here.
- BUG-056: the lock screen's background under TCG (above).
- BUG-052 (AltGr counts as Alt in `poll_key`), BUG-053 (glibc launchers keep
  their arena on an early error; four boot-time `expect`s on allocation left
  in `ring3.rs`), BUG-054 (seam in the default background), BUG-055 (Storage:
  rounding, "no virtio-blk devices"), BUG-058 (calculator below 252 px).
- `ring3.rs`: no test starts a process with zero free frames; the refusals of
  `a19d507` were read, and exercised only as far as the churn tests and the
  soak go (they never ran out of frames: `out of memory: page tables` 0 times).
- On a live medium with a blank disk the tour comes up while the install
  wizard is open (`wizard-install-test.py` closes it with Esc before its first
  screenshot); typing and clicks pass through it, as `feature/help` designed.
  Whether a medium that opens the wizard should show the tour at all is a
  product question.
- The builds are not bit-reproducible: the same commit built twice gave two
  different `eurokernel.efi` (seen when the release kernel was relinked for
  its symbols). Not looked into.
- English only, and everything else the five proofs list as open.
