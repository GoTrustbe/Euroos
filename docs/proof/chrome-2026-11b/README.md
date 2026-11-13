# Chromium on EuroOS: fd spaces per process, memory that comes back, a desktop that keeps moving (2026-11-13)

Branch `feature/chrome-fdspace`, from `feature/chrome-workplace-2` `affc864`, with
`feature/app-control` `2e127dd` (the validated `feature/chrome-workplace-2` at `197935b`)
merged in at `8346efc`. Not merged, not pushed. Bugs: `../../LOADTEST-TRACKER.md`
(BUG-020, BUG-098, BUG-120 to BUG-137).

| Commit | What |
|---|---|
| `6484b85` | BUG-098, the tests first: `[fdsem]` (glibc with fork, musl, background path). FAILED on the tree before |
| `75f254f` | BUG-020 (`[fuzz]` under KVM), BUG-120 (brk region of a foreground program) |
| `6de8770` | **Step 1, W20 / BUG-098**: one descriptor table per process; BUG-122 |
| `a7d6f62` | **Step 3, W22**: BUG-123 to BUG-129, the desktop stopwatch, the NMI symboliser |
| `3c1892c` | **Step 2, W21**: BUG-131, BUG-132, BUG-133 |
| `2374531` | BUG-130: virtio-net transmit ring |
| `8346efc` | merge of `feature/app-control` `2e127dd` (the validated branch) |
| `670dea4` | `chrome-smoke.py`: the 20-starts batch, the diagnostics of a navigation that does not happen |
| (last) | this proof, the tracker, the sprint plan, `CHROME-ON-EUROOS.md` |

The regression round ran on the images of `8346efc` (the last commit that changes the
kernel): self-test image sha256 `edbb40a26fab4eb5…`, public image for the browser runs
(`ui.tour=off;update.check=off`) `49b026c819d5bde5…`, plain public image (no boot options)
`1f94e6b1caa88391…`. `670dea4` changes only the harness.

Machines: build server .16 (TCG); Dell R630 (KVM, `-cpu host`, QEMU 10.2.1, CPUs 24-39).
Browser runs: 2 vCPU, usb-kbd and usb-tablet over QMP, virtio-net on SLIRP without
`restrict=on` (matrix sites only), `chrome-pack2.img`, `nss-pack-vid.img`. Every other
run: `restrict=on` or no NIC.

## Step 1. W20: one fd space for the browser and all its children (BUG-098)

**Cause.** A glibc process and all its fork children shared one set of tables in which a
NUMBER was the object: 500 numbers for the files, pipes and directories of all of them,
sockets from 500, AF_UNIX 600, eventfd 800, epoll 900. Five overlays (`FD_ALIAS`,
`FORK_INHERITED`, `DEFERRED_CLOSE`, `CHILD_OPENED`, `FORK_CHILD_CLOSED`) and a kernel-level
dup that made a second object pretended otherwise; `SCM_RIGHTS` made a new object per
hand-over. 245 files after euro-os.eu, 380 after Wikipedia, 483 in GitHub, EMFILE.

**Fix.** `kernel/src/fdtab.rs` + `crates/eurofdspace` (`ProcTab`, `Refs`): a descriptor
table per process (descriptor → object, close-on-exec per descriptor) and a reference
count per object. One gate in front of both dispatchers answers close, dup, dup2, dup3,
`F_DUPFD(_CLOEXEC)`, `F_GETFD/F_SETFD`, `close_range`, translates descriptor arguments, holds
a reference on the objects of a call in progress and gives a new object its descriptor.
Fork copies the table; exec closes the close-on-exec descriptors; the last close ends a
pipe end (EOF, EPIPE, POLLHUP), a socket, an eventfd, an epoll instance, in whatever
process; `SCM_RIGHTS` moves the object with a reference into the receiver's lowest free
descriptor; `/proc/self/fd`; `RLIMIT_NOFILE` 4096/4096; numbers lowest-first for every kind
of object (the object classes moved to 0x10000…0x14000). The overlays are gone.

**Shown how.** Tests first (`6484b85`): `[fdsem]` runs `gfdsem` (glibc with fork, 49
checks), `mfdsem` (musl, 25) and `fdsem` (background path, 16). On Linux 6.x the same
programs pass 49/49 and 24/24 (the musl build has one check more here: `/proc/self/fd` in
small pieces). On the tree before: glibc 12/47, musl killed (exit 139), background 3/16.
After: PASS in every boot of this round (KVM 4 GiB and 1 GiB; TCG 1 GiB; at 512 MiB the
glibc half is not run, at 1 GiB and 2 GiB glibc's fork is refused for memory and the line
says "one process only"). In the browser: 186 of 4096 file objects after euro-os.eu, 214
after Wikipedia, 220 in GitHub (`fds` after every site).

Found on the way: **BUG-122** (every Chromium child aborted "FD ownership violation": it
takes descriptor 3 as its crash-handler socket; the exec now gives it `/dev/null` there),
BUG-120, BUG-121 (open), BUG-020 (the fuzzer was not stuck: one serial line per unknown
syscall).

## Step 2. W21: YouTube `V8 process OOM (Zone)`, GitHub kernel heap panic

**What the binary said.** `[mm-fail]` (new: one line per failed mmap/brk with the state of
every pool): `mmap(len 512 KiB, PROT_RW, anon) = ENOMEM | arena mmap window 468 KiB left of
253 MiB | demand addresses 403 of 480 GiB handed out`. Anonymous mappings under 16 MiB came
from the arena window, a bump allocator that never gives back; larger ones from the demand
region, whose bump pointer never comes back. And `ramfiles` (new) in GitHub: 633 MiB in 542
files without a name (Chromium's shared-memory regions, 2 MiB each) against 13 MiB of
profile, then `memory allocation of 1105728 bytes failed`.

**Fix.** BUG-131: anonymous mappings from 64 KiB go to the demand region (a reservation
costs nothing until a page is touched); munmap gives addresses back to a per-process list
of free ranges (`euromm::FreeRanges`, host-tested against a page-bitmap model), mmap takes
from there first. BUG-132: a nameless RAM file is freed (bytes and shared frames) when no
descriptor in any fd space and no mapping in any process names it; a RAM file may not grow
into the last 32 MiB of kernel memory (ENOSPC instead of a panic). BUG-133: the browser's
arena 128 MiB instead of 256 (it uses 80-86 MiB; on 4 GiB the seventh fork failed).

**Shown how (provoked and measured).**

| | before | after |
|---|---|---|
| `V8 process OOM (Zone)` | y1 (YouTube) | none in any run since; the watch page loads at 4, 8 and 16 GiB (y4, y8, y16, f-yt) |
| demand addresses | 403 of 480 GiB within minutes | 128 of 480 GiB, 16 GiB given back, 11.6 GiB reused (y4) |
| RAM files after GitHub | 633-748 MiB | 95-122 MiB |
| kernel heap panic | s3 (GitHub) | none |
| site matrix 4 / 8 / 16 GiB guest | (died in GitHub) | 6/6, 6/6, 6/6 (a4, a8 round 1, a16) |
| fork / exec with interrupts off | 95-276 ms / 109-341 ms | 49-116 ms / 50-101 ms, in pieces with timer windows |

**Not done (the wall).** The bytes of RAM files still live in the kernel heap (≤ 512 MiB at
every guest size) and a mapped one is held twice (heap bytes and frames). Frames per file
with a guest-RAM-dependent limit is the structural step that remains (BUG-132, "Open").

## Step 3. W22: the desktop stands still 12-17 s while GitHub loads

Reproduced (g7: slowest console answer 6 822 ms; g8: 18 117 ms and `WATCHDOG TRIPPED`).
Located with NMI probes (`scripts/nmisym.py` names them from the link map of the image
that ran), the `[tick-late]` lines, and from `a7d6f62` the kernel's own stopwatch on its
desktop loop (`[desk]` for every gap over 100 ms between two passes, split by part of the
pass; `deskstat`). Causes, each shown and fixed:

| Bug | Cause | Evidence |
|---|---|---|
| BUG-126 | ~1 MB of per-event lines per GitHub load on the UART, two host exits per 16 bytes, interrupts off; the desktop loop prints the DevTools lines itself | every NMI in g8: task 0 preempted in `serial::__print` |
| BUG-123 | two port accesses per byte | — |
| BUG-125 | the harness's serial reader copied the whole log per chunk; the guest waited in its UART writes | g7/g8 stalls of seconds with the guest idle |
| BUG-127 | the loop halted with others ready, then waited a round of 30 threads | `[desk] … then 300 ms without the CPU` |
| BUG-128 | fork and exec copy/wipe 256 MiB in one piece with interrupts off | `[tick-late] +9..+12 ticks` in `do_glibc_fork`/`do_child_execve` |
| BUG-129 | the loop waited for the UART for its own lines | `[cdp] one pump call took 569 ms` |
| BUG-124 | the NMI probe itself livelocked a ring-3 NMI (SMAP fault in the handler) | g3/g4: "the same page faults again and again … 262144 times" |

**Measured** (R630, GitHub, kernel stopwatch = longest gap between two passes of the
desktop loop; console = the harness's echo every 0.25 s):

| | before | after (m7, m8, m9 three side by side; a4, a8, a16) |
|---|---|---|
| GitHub, slowest console answer | 6 822 / 18 117 ms | 91-149 ms |
| GitHub, kernel stopwatch | — | 70-104 ms |
| browser start to first page | 36.7-37.4 s | 12.6-13.9 s |

Over 100 ms remain, none of them a guest system call: the `chrome` shell command itself
(one pass of 500-800 ms), full desktop repaints at start-up (110-160 ms), and during busy
pages up to 104-131 ms "without the CPU" with 30-40 ready threads (the timer turn of the
loop arrives, but the late ticks of `do_child_execve` (≤ 50 ms) and a busy host add to it).

## Step 4. The sprint's exit criteria, measured on `8346efc`

| # | Criterion | Result | Numbers |
|---|---|---|---|
| 1 | every site, three rounds in one session | **not met** | one round: 6/6 at 4, 8, 16 GiB (a4, a8, a16; also e1/e2: pdf, euro-os, Wikipedia, 2 rounds, 9/9). Three rounds: d1, d2 fail in round 2: after the PDF site a renderer aborts at its start (BUG-134) and typed URLs no longer navigate (BUG-135) |
| 2 | interaction matrix through real input | **not met in the soak** | in the 2-hour session every interaction step after the PDF site failed for the same reason (BUG-134/135); the 20-starts batch: 20/20 (typing right after start, omnibox, menu) |
| 3 | YouTube 2 min, picture and sound, real click | **not met, not measurable here** | the watch page loads (no V8 OOM); YouTube answers this network with "Sign in to confirm you're not a bot" (y8c) or keeps its consent dialog (f-yt). No login, no workaround |
| 4 | 2-hour run, matrix repeated | **not met** | the guest stayed up the full 2 h (6 rounds, no kernel fault); the browser itself was killed in round 4 (~74 min): the demand pool went from 2 834 MiB free to 0 over three rounds and a page fault of the browser's main thread could not be served (`[isolation] … task 1 TERMINATED`); from round 2 on the steps after the PDF site failed (BUG-134/135) |
| 5 | reference site < 3 s; key to screen < 100 ms | **half** | load: median 1.33 s (p1) and 1.66 s (f-perf). Key to screen: the harness step registers again (30 of 30 keys): median 292 ms / 335 ms, p90 406 / 605 ms; **not met** |
| — | 20 starts | **met** | 20/20 PASS (after the harness fix of `670dea4`: 11/20 before, all harness) |

## 5. Regression round

| Check | Result |
|---|---|
| `cargo test` on the host (`cargo-test.txt`) | 187 test binaries, 1 347 tests passed, 0 failed (`euromm::vaspace` new: 5) |
| Self-test image, TCG matrix of 10 (`tcg-boot.sh`, `tcg-matrix.sh` of `../integ-2026-11b`) | **9 of 10** to the desktop. `user-smp4-iommu` (512 MiB) went silent in the SDL test and the NMI found the scheduler lock wedged (BUG-136); repeated once with the same image: desktop (`tcg-repeats.txt`) |
| `markers-compare.py` against `../linux-rand-2026-11/` and `../validate-2026-11c/` (`comparison-tcg.txt`, 20 comparisons) | **not 0 worse**: `fork FAILx7 → FAILx8` in the four 1 GiB storage/combined configurations: that is the new `[fdsem]` asking for a fork, which 1 GiB refuses (its verdict says "one process only", PASS); and `[3g2-wire] WATCHDOG TRIPPED` once in `kernel-smp1`: one pass of the desktop loop took 57 s under TCG (the periodic integrity check or the scrub; BUG-085 family). It came back in 1 of 3 further TCG boots (the repeat of `user-smp4-iommu`, a 25 s first pass), not in the two repeats of `kernel-smp1`. Nothing gone; new in each: `[fdsem]` OK (and `[rename]` against linux-rand) |
| `[lk]` | 0 order inversions in every boot that printed it (TCG, KVM, short boots, soak) |
| CI q35 matrix (`run-metal-matrix.py`, 13 legs), run with `restrict=on` on every NIC (`q35-matrix.txt`) | **11 of 13** PASS. `scan` and `printer` FAIL: their guest talks to mock servers on the host through SLIRP's 10.0.2.2, which `restrict=on` blocks (a `guestfwd` to them is refused by QEMU at start-up when nothing listens yet). The validation ran this matrix without `restrict` (13/13); here the network rule of the task was kept |
| `gui-smoke.py` on the public image (no boot options, 2 vCPU, 4 GiB, `restrict=on`) | **PASS 67/67** |
| R630, 100 short boots of the self-test image (2 GiB, smp 2, no NIC, each to the `[rename]` verdict) | 100 of 100 reached it; `[3d8]`, `[3d7]`, `[3d3]` FAILED in 0; seeded 100, unseeded 0; `[linux-rand]` OK 100; `[rename]` OK 100; `[fdsem]` OK 100; `[fuzz]` OK 100 (the validation's KVM boots printed `[fuzz]` FAILED in every boot: BUG-020); no crash line. One FAILED line per boot: `[fork] arena alloc FAILED (384 MiB, pool has 288 MiB)`, the fork of `[fdsem]` that 2 GiB refuses (verdict "one process only", PASS) |
| R630 soak, 2 h 05 (`loadtest.py --kvm --smp 4 --nic e1000e --restrict --nmi-on-hang --mem 16G --disks 8G --host-http --hostfwd 18791 --hammer 4 --procs 32 --scenario soak --soak-churn-proc 8 --duration 7500`, self-test image) (`soak-summary.txt`) | **completed clean**: 3 741 of 3 741 commands ok (479 `churn proc`), 2 760 `[churn-proc]` verdicts, 0 FAILED; heap +0.24 kB/min after 10 min, range 1 262 kB (previous: 2 744/2 744, +0.17 and +0.19 kB/min, 1 282 and 1 284 kB); tasks, files, kstacks, process pool flat; httpd 886 548 answered, 0 failed, p50 5.0 ms, p99 152 ms; no panic, GP, page fault, NMI. FAILED lines: the one `WATCHDOG TRIPPED` of every soak (BUG-085) and the boot self-tests of the network driver that cannot reach the internet with `restrict=on`, as in the base. Markers of its boot against `../validate-2026-11c/r630-soak-f98da76-boot.markers`: **0 worse**, `[fuzz]` FAILED → OK, new `[fdsem]` OK (`soak-compare.txt`). `GLIBC-DYNAMIC-OK` is counted 24 times instead of 2 831: that line is now kept off the console (BUG-126) and is in `dmesg` |
| `scripts/boot-time.py`, plain public image, 5 boots (`kvm-boot-time.txt`) | 2.75, 2.67, 2.47, 2.53, 2.68 s to the desktop (median **2.67 s**; the validation measured 2.76 s for its commit and 2.80 s for the base side by side, the round before 2.55 s) |


## 6. What stays open, with its wall

- **BUG-134** a renderer aborts at its start after the PDF viewer has run (stack, no
  message). Not caused by the address reuse (the same with reuse disabled, run x1);
  not caused by the RAM-file sweep alone (x2 never got that far). Next: symbolise the
  stack against the pack.
- **BUG-135** typed URLs do not navigate after BUG-134 (and lost 2 characters once).
  The browser's main thread is idle and the X queue empty when it happens (`cdiag`).
- **BUG-136** one TCG boot (smp 4, 512 MiB) wedged on the scheduler lock in a page
  fault with CR2 0x1000 while the SDL test (64 MiB fallback arena) ran; not reproduced
  in a repeat. The NMI probe deadlocks itself there (`sched::current()`).
- **the demand pool drains** during the browser soak (2 834 MiB free → 62 MiB after
  three rounds → 0 in round 4, then the browser is killed on a fault it cannot be given
  a page for; BUG-137); the disk page cache (`DISK_PAGE_CACHE`, never evicted) is one candidate.
  Not attributed: a census of demand frames by owner is the next measurement.
- key to screen 292-335 ms; RAM files in the kernel heap (BUG-132); YouTube video.

## 7. Deviations from the rules

- **The q35 matrix ran with `restrict=on`** (11/13; scan and printer need the host's
  mock servers through an unrestricted SLIRP). A first attempt with `guestfwd` hung on
  its first leg and was stopped.
- **One experiment image ran a minute on CPUs 22-23** (run x2), which belong to the
  validation team; it was stopped and restarted on 38-39.
- **Images and test scripts travelled through the laptop** (streamed with ssh/gzip,
  not stored there except screendumps and small result text files, as allowed). No
  source code on the laptop: patches were Python scripts that edit the tree on .16.
- **musl**: `mfdsem` was compiled on the R630 (`6484b85`; .16 has no musl-gcc); the
  source was removed after each compile.
- Two experiment images (address reuse off, RAM-file sweep off) were built from
  temporary edits that were reverted before any commit; they are not measuring points.
- The browser runs used `chrome-pack2.img` and `nss-pack-vid.img` copied from
  `/srv/euroos/chrome-runs/`; that directory was not changed.
- The 20-starts batch and the site matrix were judged with harness fixes made during
  the round (`670dea4`, and the consent dialog in `3c1892c`); every harness change is
  in a commit.

## Files

`cargo-test.txt` · `tcg-*.markers`, `comparison-tcg.txt`, `tcg-summary.txt`,
`tcg-problems.txt`, `tcg-repeats.txt` (TCG matrix and its repeats) · `q35-matrix.txt` ·
`gui-smoke-public.txt` · `r630-short-boots.txt` · `soak-summary.txt`,
`r630-soak-boot.markers`, `soak-compare.txt` · `kvm-boot-time.txt` · `chrome-runs.txt`
(every browser run named above: image size, aborts, connections closed with
unacknowledged data, one line per step) · `starts20.txt` · `chrome-soak.txt`.
Serial logs stay on the build servers.
