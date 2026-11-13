# Validation of `feature/chrome-workplace-2` before the merge (2026-11-12 and 13)

The full test round on the branch of the Chromium work, plus two aimed checks: the
`rename` fix of BUG-091, because it touches file data, and the X server, because every
hosted app uses it. Not merged, not pushed. Bugs: `../../LOADTEST-TRACKER.md`
(BUG-110 to BUG-115).

| Commit | What |
|---|---|
| `affc864` | the branch as it was handed over (it contains `feature/app-control` `c6f033f`) |
| `de92f08` | `userland/renprobe.c`, the `[rename]` boot self-test, `/bin/renprobe`. On this commit the self-test prints FAILED: BUG-110, measured |
| `7be30cb` | **BUG-110**: a file that is still mapped keeps its bytes when it is unlinked or replaced (a regression of BUG-091's fix) |
| `f98da76` | **BUG-113**: a hosted X app starts with an empty X server (a regression of BUG-097's fix) |
| (last) | this proof, the tracker, `scripts/rename-persist.py` |

**Which image was tested.** The round ran twice. Round 1 on the images of `7be30cb`
(self-test image sha256 `7cdaf5b1ebcdc8c5…`, public image `450544f667d56c0c…`); the X
check of that round found BUG-113. The round was then repeated in full on the images of
`f98da76` (self-test image `45f39ff5d1577aa3…`, public image `38883670b3f1fc41…`), the last
commit that changes code. The tables give the second round; the files of the first are
in `round1/` and in the files named `…-7be30cb…`. "Base" is `c6f033f` plus the test commit
`de92f08` and nothing else, built in a throwaway worktree (`images.sha256`). Serial logs
stay on the build servers (`*.log` is not committed).

## 1. Verdict

The branch is ready to merge, with the two regressions this round found fixed on it
(`7be30cb`, `f98da76`). Of what the round asks, everything is green on the last code
commit except one run: the virtio monkey soak is 78/79 because the first-run tour once
ignored Esc, a fault the base shows more often (BUG-115). Two things that the task asked
to check do not hold, on the base nor on the branch, and are open, not caused here:
a rename **by a Linux-ABI program** does not reach the disk (BUG-111), and the GTK demo
window draws but takes no input (BUG-112).

BUG-091 itself: the scenario fails on `c6f033f` from the first rename on (20 and 24
checks) and passes on the branch (39 and 47 checks), in every one of 222 boots; on an
installed system the shell's renames survive a restart and a power cut with a clean
`scrub`.

## 2. BUG-091: rename over an existing file, before and after

### The scenario

`userland/renprobe.c`, one source, two builds (freestanding for `/bin/renprobe`, and
against glibc; on Linux the glibc build passes every check). Sixteen small files and
three of 1 MiB; descriptors held open on the eight later small files, each at its own
offset; a shared mapping of one of them; a private mapping of a 1 MiB file of which one
page is read. Then:

- **A** a new file is renamed over an EARLIER file, three times (how a program saves a
  document);
- **B** the same over a file that is open: the descriptor must keep the old bytes, the
  path must give the new ones;
- **C** six saves with a backup (unlink, rename, rename), and an unlink of a file in the
  middle;
- **D** a file that is mapped and whose descriptor is closed is replaced, another one is
  unlinked: the mappings must keep the bytes they were made of.

After every step every file is read by its path, every held descriptor is read in full
(bytes, offset, size), a write goes through one of them and is looked for in its own
file, and the mappings are compared. One line per check.

The `[rename]` boot self-test (`kernel/src/renametest.rs`) runs the freestanding build
as a foreground program and the glibc build with demand paging, where a mapping of 1 MiB
is filled from the file page by page: a page first read after the rename shows which
file the mapping belongs to.

### Result

| | base (`c6f033f` + the test) | `de92f08` (the branch + the test) | `7be30cb` and `f98da76` |
|---|---|---|---|
| `[rename]`, freestanding run | 19 checks ok, **20 failed** | 39 ok, 0 failed | 39 ok, 0 failed |
| `[rename]`, glibc run with demand paging | 23 ok, **24 failed** | 45 ok, **2 failed** (step D) | 47 ok, 0 failed |
| verdict line | FAILED | FAILED | **OK** in every boot of both rounds (the 20 matrix boots, the 200 short boots, the 2 soak boots) |
| the whole scenario typed in the Terminal of the installed public image | 19 ok, **20 failed** | | 39 ok, 0 failed |

What fails on the base, from the first rename on (`rename-selftest-c6f033f.txt`):
`A1 every file by its path FAILED` (the file behind the replaced one reads another file's
bytes), `A1 every held descriptor FAILED` (from the first held descriptor on),
`A1 a write through a held descriptor lands in its own file FAILED` (the 13 bytes were
written, into a neighbour), `A1 the mapping of big1, pages first touched after the rename
FAILED where 4097` and `B the open descriptor still reads the replaced file FAILED`. That
is BUG-091 as chrome met it. On `de92f08` all of that passes; what is left is step D,
BUG-110 (section 3).

### On an installed system, across a restart

`scripts/rename-persist.py` (R630, KVM): the public image installs itself on a blank
4 GiB disk through the wizard, the installed disk is booted three times.

| Step | base | `7be30cb` | `f98da76` |
|---|---|---|---|
| shell (`write`, `mv` over an existing file twice, `rm` + `mv`, a save with a backup three times, `rm`): no command refused, every file reads back | ok | ok | ok |
| `scrub` after it | HEALTHY, errors 0 | the same | the same |
| `reboot`; second boot: every file of the shell scenario reads back (13 files, 4 names gone) | ok | ok | ok |
| `scrub` after the restart | HEALTHY, errors 0 | the same | the same |
| one more `mv` over an existing file, power cut five seconds later (QEMU killed); third boot: the file holds the new text, everything else as before, `scrub` | ok, HEALTHY | ok, HEALTHY | ok, HEALTHY |
| `renprobe save DIR 2` in the Terminal (a Linux-ABI program: rename over `a.txt`, unlink + rename of `b.txt`, unlink of `c.txt`): the program reads back what it saved | ok | ok | ok |
| the shell's `cat` and `ls` show that save | **no**: version 1 of all three | **no** | **no** |
| after the restart | **no**: version 1 of all three | **no** | **no** |
| checks | 20 of 24 | 21 of 24 | 21 of 24 |

So: a rename through the kernel's own file system (the shell, the desktop apps) is
atomic, survives a restart and a power cut, and leaves a clean `scrub`. That path is not
touched by this branch and behaves the same before and after. A rename or an unlink by
a **Linux-ABI program** is not written to the disk at all, on the base as on the branch:
BUG-111, open, not caused by this branch and not fixed here. The fourth failed check of
the base is the whole scenario in the Terminal (BUG-091 itself).

## 3. Regressions found by the round, and fixed

**BUG-110** (`7be30cb`). BUG-091's fix frees the bytes of a file that lost its name when
nobody can reach it any more, and did not count a mapping that is filled page by page,
nor the mappings of the other processes. "Map, close, unlink" and a save over a mapped
file then read zeros in every page not touched before. Before BUG-091's fix an unlinked
file kept its bytes for good, so the unlink half came with that fix. Found by step D of
the self-test on `de92f08`; `file_referenced` now looks at the lazily filled mappings and
at the lists parked in `CHILD_MEM`.

**BUG-113** (`f98da76`). Since BUG-097 the frame of a hosted X app shows every mapped
window of every connection. A client that ends without closing its socket stays
connected with its windows mapped: all six X clients of the boot self-tests, and an app
closed with the close button of its frame. `app run hello-x11` on the self-test image
came up with the Pango demo of the self-tests drawn over it, and the key it waits for
went to a dead client's window (`screenshots/x11-app-7be30cb-before-bug113.png`,
`x11-app-7be30cb.txt`). On the base the same frame shows the app alone
(`screenshots/x11-app-c6f033f.png`). In the public image the first hosted app of a boot
is not affected, a later one is. Fix: the desktop empties the X server right before it
starts a hosted app (at most one runs at a time).

After `f98da76` (`x11-app-f98da76.txt`, `screenshots/x11-app-f98da76-first-start.png`):
six dead connections forgotten at the first start; the frame shows the app's own
window; a key typed on the USB keyboard arrives as KeyPress on its window, the app
destroys the window and ends; started a second time it is alone again; closed with the
close button and started a third time, the server holds that one connection.

## 4. Found, not caused by the branch, not fixed

- **BUG-111**: a rename or an unlink by a Linux-ABI program does not reach the file
  system on disk (section 2). The same on `c6f033f`. This is the one place where "the
  rename must survive the restart" does not hold, and it does not hold for any Linux-ABI
  program that saves by renaming, started from the Terminal.
- **BUG-112**: the GTK demo of the feature `live-demos` (section 5).
- **BUG-114**: a hosted app that ended by itself leaves its frame on the desktop with its
  last picture, and `app run` answers "a hosted app is already running" until the frame is
  closed. A second start of an app is not resized to its frame (the first start is).
  Seen with `hello-x11`; small.

## 5. The X server

- **The X11 self-tests of the boot suite** (`gxwin`, `gcairo`, `gcairotext`, `gpango`,
  `gxlive`, SDL): the same lines and exit codes on the base and on the branch, 282 lines
  compared (`x-selftests-base-vs-branch.txt`). What differs is the trace: the key lines
  of the new decoder, the 128-byte warning that is gone (BUG-095), and one MotionNotify
  that the base sent before a ButtonPress and the branch does not. `gxwin` is PARTIAL
  (keys=3 button=0, exit 1) and `gxlive` counts keys=0 clicks=0 on both: not new.
- **A third-party X app in a hosted window** (`app run hello-x11`, the package the
  `[app]` self-test installs): section 3. Drawing: correct on `f98da76`, as on the base.
  Input: a key from the USB keyboard and a click from the USB tablet reach the app on
  the branch (KeyPress and ButtonPress on its window in the X trace; the app destroys its
  window). Whether they did on the base cannot be said: the base has no X trace, the
  frame stays after the app ends either way (BUG-114), and the chrome round measured
  the base's input as BUG-092.
- **The GTK demo window (`ggtk`)** exists only in an image built with
  `--features live-demos`, which no default build has, and there is no GTK test in the
  boot suite. Built for the base and for the branch (`gtk-demo.txt`):

  | | base `c6f033f` | `7be30cb` | `f98da76` |
  |---|---|---|---|
  | starts in which the app loaded its libraries | 7 of 7 | 6 of 10 | 4 of 5 |
  | the window, when it loaded | the GTK window: title, `tick 0`, entry, button (`screenshots/gtk-demo-c6f033f.png`); in the first run an empty white body | the frame shows the Pango demo of the self-tests (BUG-113, `screenshots/gtk-demo-7be30cb-before-bug113.png`) | the GTK window, as on the base (`screenshots/gtk-demo-f98da76.png`) |
  | `GGTK: live tick` lines (the app's one-second timer) | 0 in every run | 0 in every run | 0 in every run |
  | the demo test's typed `hi` and its click on Reset | delivered, never read (`160 B of input queued unread`) | the keys went to a dead client's window; the click was delivered, never read | delivered to the app's connection (704 bytes queued), never read |

  So a GTK window **draws** on the branch as it does on the base, and it does **not take
  input** on either: the events reach its connection and stay there, because the app
  stops after its first frame (its own timer never fires). And the app does not always
  get that far: the demo is started while the boot still runs the self-tests that come
  after the glibc suite; in the three failed starts whose logs were kept the loader's error
  (`liblz4.so.1: failed to map segment from shared object`, `libcap.so.2: cannot read file
  data`) comes right after the `[app]` self-test launches its own glibc program. That
  self-test and the launch are the same on the base; whether the branch made the
  collision more likely is not established (0 of 7 starts against 5 of 15 is what was
  counted). BUG-112, open, not analysed further; the demo is in no image that is built by
  default.
- **USB keyboards send F3 to F12 and Caps Lock now** (`xhci.rs`, for the browser). On the
  public image, pressed between commands typed in the Terminal: nothing happens, the
  commands arrive as typed, also "under" Caps Lock (`usb-function-keys.txt`).
- `cdiag`, `xtrace on|off`, `fstrace N`, `ctabs` on the desktop with no browser running:
  each answers and the desktop goes on (`x11-app-f98da76.txt`).
- `update.check=off`: read, not run. Without the option nothing changes (one early return
  in `update::maybe_check`); the live image has no slots and never checks anyway.

## 6. The round

### Build server (QEMU 10.2, TCG)

| Test | Result on `f98da76` |
|---|---|
| `cargo test` (default members) | 187 suites, 1 331 passed, 0 failed, 1 ignored (`hosttests.txt`); the same counts as the chrome round: this round adds no host test, its tests are the probe, the boot self-test and the scenario script |
| Self-test image, TCG matrix of 10 (`tcg-boot.sh`, `tcg-matrix.sh` of `../integ-2026-11b`) | 10 of 10 to the desktop, no fault line, no NMI probe. `markers-compare.py` against `../linux-rand-2026-11/` and against `../integ-2026-11b/` (`comparison-tcg.txt`, 20 comparisons): **0 worse, 0 changed**, nothing gone; new in each: `[rename]` OK (and `[linux-rand]` OK against integ-2026-11b) |
| `[lk]` in the matrix | `[lk] boot-total order inversions=0 (self-test provoked=2) task-lock-with-IF0=0` in 10 of 10 (`lock-tcg-lines.txt`) |
| `[rename]` in the matrix | OK in 10 of 10; the six 512 MiB legs with the freestanding run only (39 checks; "glibc not run: (no arena for glibc)", as for the other glibc self-tests at that size), the four 1 GiB legs with both runs (39 and 47 checks) (`tcg-newtests.txt`) |
| CI q35 matrix (`run-metal-matrix.py`, 13 legs) | **13 of 13** PASS (`q35-matrix.txt`) |
| `megaraid-proof.sh`, with IOMMU and with `NO_IOMMU=1` (4 boots) | 0 markers worse than `../integ-2026-11b/`, 0 changed, no fault line (`comparison-mr.txt`, 4 comparisons); host checks 4 of 4 in both |
| `scripts/repro-build.sh`, twice, the second after removing the `.efi` and touching `main.rs` | **REPRODUCIBLE**: kernel `3ad516e5d49b…`, loader `be6abd24a7d4…` both times; 0 paths of the build machine in the kernel (`repro-build.txt`) |
| Public image, `gui-smoke.py` (usb-kbd + usb-tablet, smp 2, 2 GiB), no NIC | **PASS, 67/67** (`gui-smoke-tcg.txt`) |
| Public image, `gui-smoke.py --net e1000e` | **PASS, 67/67** (`gui-smoke-tcg-e1000e.txt`) |
| Public image, `wizard-install-test.py` | **15/15** (`wizard-install-test.txt`) |
| Public image on the open network, virtio-net and e1000e: `net`, `ping`, `nslookup`, `fetch`, `https example.com`, `netstat` | all answered on both; TLS 1.3 handshake and 892 bytes over https on both; no self-test line in the public image (`network-open.txt`) |

**67 or 66 steps.** The chrome round reported `gui-smoke.py` 66 of 66 where the rounds
before it had 67 of 67. The step that is not in its list is "first-run tour: shown on
the first desktop, Esc closes it". `gui-smoke.py` records that step only when the image
shows the tour, and the chrome round's image was built with
`EUROOS_BOOTOPTS="ui.tour=off;update.check=off"` (its README says so). Nothing was lost:
on the public image as it is built for release the step is there and passes, 67 of 67 in
all four runs of this proof.

### Dell R630 (KVM, `-cpu host`)

Only built images and test scripts went there (`/srv/euroos/validate-runs`); no source
tree, no key. Every QEMU with `-nic none` or SLIRP `restrict=on`, pinned, at most
14 vCPUs of ours at a time; no VFIO, no PCI device touched.

| Test | Result on `f98da76` |
|---|---|
| 100 short boots of the self-test image, each to the `[rename]` verdict (`short-boots.sh`: 2 GiB, smp 2, no NIC) | `[3d8]`, `[3d7]`, `[3d3]` **FAILED in 0**; **unseeded 0**: one `[entropy] CSPRNG seeded` line in 100 of 100 (488 to 563 credited bits); `[linux-rand]` OK in 100; `[rename]` OK in 100, each with the glibc run and step D; no crash line. One FAILED line per boot: `[fuzz] … kernel-survived=false`, which every KVM boot of the self-test image prints, on the base as well (`r630-short-boots-f98da76.txt`) |
| `scripts/boot-time.py`, public image, 5 boots (SATA medium, smp 2, 2 GiB, `restrict=on`), alternating with the base | final: 2.67, 2.68, 3.05, 2.88, 2.76 s to the desktop (median **2.76 s**), kernel entry to desktop 894 to 956 ms (median 916 ms). Base, in between: 2.63, 2.91, 2.76, 2.80, 2.93 s (median 2.80 s), 891 to 953 ms (median 919 ms). No difference (`kvm-boot-time.txt`). The round before reported 2.55 s; measured side by side today the base gives 2.80 s and this commit 2.76 s |
| Monkey soak 35 min, USB input, smp 4, 4 GiB, no NIC, seed 20261114 | PASS: smoke 67/67, then 2 047 actions, every check ok (78 steps of 78), no crash signature, console max 300 ms (`gui-soak-usb-f98da76.txt`) |
| Monkey soak 35 min, virtio input, smp 2, 2 GiB, `restrict=on`, seed 20261115 | **FAIL, 78/79**: every step and the 35 minutes of monkey (2 286 actions, no crash signature, console max 2 054 ms) ok except the first one, "first-run tour: … Esc closes it": the Esc reached the kernel, the tour closed only on the next click. Not a regression: repeated five times each on this image and on the base, the base failed it 2 of 5 times, this image 0 of 5 (`tour-virtio.txt`, BUG-115). Round 1 (`7be30cb`): PASS 79/79 |
| NVMe: the public image booted beside a 64 MiB NVMe disk of random data | sha256 before and after equal, `NVME-UNTOUCHED` (`nvme-check.txt`) |
| Boot of the soak guest against `../integ-2026-11b/r630-soak-boot.markers` and `../linux-rand-2026-11/r630-soak-boot.markers` | 0 worse, 0 changed, nothing gone, in both; new: `[rename]` OK (and `[linux-rand]` OK against integ-2026-11b) (`comparison-r630-soak-boot.txt`) |
| Soak, 2 h 05 | below |

### Soak, 2 h 05

`loadtest.py --kvm --smp 4 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G
--host-http --hostfwd … --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8
--duration 7500`, self-test image, `net.driver=user`.

| | `f98da76` | round 1, `7be30cb` | previous soak (`../integ-2026-11b/`, `6fcf24c`) |
|---|---|---|---|
| commands | **2 744, all ok**, `completed clean` | 2 556, all ok, `completed clean` | 2 744, all ok |
| `churn proc 8` | 351 | 327 (median 6 182 ms) | 351 |
| glibc launches (`GLIBC-DYNAMIC-OK`) | 2 831 | 2 639 | 2 831 |
| `[churn-proc]` verdict lines | 2 024, 0 FAILED | 1 886, 0 FAILED | 2 024, 0 FAILED |
| heap slope after 10 min (least squares) | **+0.19 kB/min**, range 1 284 kB | +0.29 kB/min, range 1 301 kB (+0.76, +0.43, +0.11, -0.42 in the four half hours) | +0.17 kB/min, range 1 285 kB |
| tasks, files, kernel stacks, process pool after 10 min | flat (57, 249, 0, 887 296) | flat | flat |
| panics, ring-0 faults, NMI probes | 0 | 0 | 0 |
| httpd during the soak (4 clients) | 920 144 answered, 0 failed, p50 4.9 ms, p99 123.5 ms | 866 103, 0 failed, p50 5.4 ms, p99 132.3 ms | 917 858, 0 failed, p50 4.9 ms, p99 123.4 ms |
| `[rename]` in the soak guest | OK | OK | (not in that tree) |
| FAILED lines after the desktop | one: `[3g2-wire] WATCHDOG TRIPPED` (BUG-085, in every soak) | the same one | the same one |

`r630-soak-*-summary.txt`, `r630-soak-compare.txt`. The files column (slots of the RAM file table, `ring3::files_count`, a slot is never
given back) is 249 against 187 before: the slots the two `[rename]` runs of the boot
leave, live files and tombstones of the replaced and unlinked ones (the bytes of the
latter are freed; BUG-091). Constant over the whole soak.

### Round 1 (`7be30cb`)

The same round on the images of `7be30cb`, before BUG-113 was found. Everything below
held there as well, so the BUG-113 commit (`f98da76`, 36 lines in `xserver.rs` and
`main.rs`, run only when a hosted app is started) was put through the whole round again.

| Test | `7be30cb` |
|---|---|
| host tests | 187 suites, 1 331 passed, 0 failed, 1 ignored |
| TCG matrix | 10 of 10, 0 worse, 0 changed, `[lk]` 0 inversions, `[rename]` OK 10 of 10 (`round1/`) |
| q35 matrix | 13 of 13 |
| `megaraid-proof.sh` | 0 worse, host checks 4 of 4 in both |
| `repro-build.sh` | REPRODUCIBLE (kernel `3f581a0f4ea2…`) |
| `gui-smoke.py`, no NIC and `--net e1000e` | 67/67 and 67/67 |
| `wizard-install-test.py` | 15/15 |
| open network, both NICs | all answered, TLS 1.3, 888 bytes |
| 100 short boots | `[3d8]`/`[3d7]`/`[3d3]` FAILED in 0, unseeded 0, `[linux-rand]` OK 100, `[rename]` OK 100 (`r630-short-boots-7be30cb.txt`) |
| `boot-time.py`, 5 boots | 2.55, 3.11, 2.65, 2.70, 2.74 s (median 2.70 s) |
| monkey soak USB, 35 min | PASS 78/78, 2 069 actions, console max 250 ms |
| monkey soak virtio, 35 min | PASS 79/79, 2 276 actions, console max 200 ms |
| NVMe | `NVME-UNTOUCHED` |
| soak 2 h 05 | above |

## 7. Not green, not tested

- **The virtio monkey soak of `f98da76` is 78/79**: the first-run tour once ignored Esc
  (BUG-115). Measured as not caused by the branch (base 2 of 5, branch 0 of 5 in repeat
  runs), but the run itself is red and is reported as such.
- **The rename of a Linux-ABI program does not survive a restart** (BUG-111): the
  task's "the rename must survive the reboot" holds for the kernel's own file system
  (shell, desktop apps; also a power cut) and does not hold for a Linux program in the
  Terminal. On the base it does not either.
- **The GTK demo window draws but does not take input** (BUG-112): on the base it does
  not either. The X11 self-tests of the boot suite are unchanged, and a third-party X app
  (`hello-x11`) draws and takes a key and a click.
- **Not tested here:** Chromium itself (the chrome round's own proof covers it; it needs
  the packs and was not repeated); the hosted browser after BUG-113, i.e. whether
  emptying the X server before the start affects chrome (it starts with no other client,
  so it should not); `scrub` after a power cut during a rename by the shell (the cut came
  five seconds after it); the `[rename]` scenario with fork children (the glibc probe is
  one process).
- `gui-smoke.py` 66 against 67: explained above, nothing missing.

## 8. How the base images were built, and one trap

The base needs the test commit and nothing else of the branch. It was built in a
throwaway detached worktree (`base-build.sh`, `base-live.sh`, `base-st.sh`) that shares
the `target` directory of the branch worktree, because the disk has room for one. Cargo
names the artefacts of a workspace crate by the path inside the workspace, so the two
worktrees write the same files, and it judges freshness by file time: after the first
base build the branch worktree took the base's `eurofdspace` for its own and the next
branch build failed (`no method named any`: the method BUG-091's fix added). It failed
loudly, and no image of this proof was built in that state: the round-1 images are older
than the first base build, and every later branch image was built after the differing
sources were touched. The scripts do the base build and the touch under the build lock
now. Worth knowing for whoever builds two commits from one `target` directory.

## Files

`rename-selftest-*.txt` the `[rename]` lines on the base, on the test commit and on the
fix · `rename-persist-*.txt` the installed-system scenario on the three commits ·
`x11-app-*.txt`, `gtk-demo.txt`, `x-selftests-base-vs-branch.txt`, `screenshots/` the X
check · `tcg-*.markers`, `comparison-tcg.txt`, `tcg-summary.txt`, `tcg-newtests.txt`,
`lock-tcg-lines.txt` the matrix · `mr-*`, `comparison-mr.txt` · `q35-matrix.txt` ·
`repro-build.txt` · `hosttests.txt` · `gui-smoke-tcg*.txt`, `wizard-install-test.txt`,
`network-open.txt` · `r630-short-boots-*.txt` · `kvm-boot-time.txt`,
`kvm-boot-time-lines-f98da76.txt` · `gui-soak-*.txt`, `gui-soak-serial-summary.txt` ·
`nvme-check.txt` · `r630-soak-*` (summary, csv, ltstat, boot markers),
`r630-soak-compare.txt`, `comparison-r630-soak-boot.txt` · `images.sha256` · `round1/`
the build-server files of the first round · the scripts that ran it all
(`run-16-*.sh`, `r630-start.sh`, `r630-final.sh`, `collect*.sh`, `build.sh`, `base-*.sh`,
`live-build.sh`, `gtk-*.sh`, `gtk-live.py`, `drive.py`).
