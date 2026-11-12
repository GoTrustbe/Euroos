# Kernel debug round: fd spaces, failed starts, lock screen, reproducible builds (2026-11-12)

Branch `feature/kdebug` on `feature/app-control` `7858b72`. Not merged into
`feature/app-control`, not pushed. Bugs: `../../LOADTEST-TRACKER.md`.

| Commit | What |
|---|---|
| `0009b95` | **BUG-063**: the foreground exit flag is consumed by the exit path (found by the self-tests below) |
| `045b856` | self-tests `[fdspace]`, `[bug053]`, `[bug063]` and their program `userland/pipehold.c`; the first two fail on this commit |
| `cb65a7e` | **BUG-060**: fd spaces, one set of descriptor tables per process group; crate `eurofdspace` |
| `082afd1` | **BUG-053**, **BUG-061**: a failed process start gives back what it took; no vDSO tables is a refusal |
| `6022a8b` | **BUG-056**: the lock screen's background in row runs, pixel-identical |
| `dbf8460` | **BUG-062**: the same commit gives the same `eurokernel.efi`; `repro-build.sh`; the kernel's link map |
| `ff8d562` | **BUG-064**: the `[m43-wire]` self-test counts its USB audio packets with interrupts off (found by the CI q35 matrix of this round) |
| (last) | this proof, the tracker, `BOOT-TIME.md`, `CRA-CONFORMANCE.md` |

Every run below used the images built from the tree of `ff8d562` (the commit
after it changes documents only), unless it says "first round": self-test
image sha256 `232a6c18…`, release image `8f8f5783…`. Serial logs stay on the
build servers (`*.log` is not committed); the `.markers` files hold the
verdict lines of each log plus its `[lk]`, `[fork]` and `[boot-time]` lines.

`kernel/src/main.rs` is not touched: the three new self-tests run from
`ring3::spawn_window_selftest`, which `main.rs` already calls.

## 1. Descriptor tables per process (BUG-060)

**What was wrong.** `OPEN_FDS`, `OPEN_DIRS`, `PIPE_FDS`, the pipes, their
blocking flags and waiters, and the per-fd status flags were one global set.
Every launch (`run_args`, `run_glibc`, the disk and persistent variants,
`run_dynamic`, `run_interp`) begins with `reset_fd_table`, also when it then
finds no arena. A background process alive at that moment lost every
descriptor: a write to its pipe became console output, a read returned 0, an
`lseek` on its open file returned -1, and its next `open` was given the lowest
number, which it still believed to be its pipe. BUG-037 had made the reset
atomic. Two more leaks between processes sat in the same tables: a close in
one process reset the `F_GETFL` flags of that fd number for all, and a dead
background process kept its fd numbers occupied until the next launch.

**Shown first** (`045b856`, before the fix; `before-fix-selftests.txt`,
`before-fix-tcg-*.markers`):

```
[bg-out] pid 263681 /bin/pipehold: pipehold: phase1 pipe=ok file=ok fds r=3 w=4 f=5
[bg-out] pid 263682 /bin/pipehold: pipehold: phase1 pipe=ok file=ok fds r=6 w=7 f=8
[bg-out] pid 263682 /bin/pipehold: B2pipehold: phase2 pipe=BAD (write=2 read=0) file=BAD (lseek=-1 read=0)
[fdspace] 2 background processes keep a pipe and a file open across a foreground launch (musl exit=0, glibc exit=42): phase1=true ended=true pass=0/2 | … → FAILED
```

(The second process got fds 6, 7 and 8: one table for both. The `B2` in front
of `pipehold:` is the token it wrote to its pipe, on the console.)

**Design.** A *space* owns descriptors.

- `ROOT`: the launcher (task 0), kernel tasks, and the one foreground or glibc
  process the launcher runs at a time, with its threads and fork children.
  `reset_fd_table` clears this space only.
- One space per background process (`spawn_bg_musl`, `spawn_bg_app`: the musl
  processes, native apps, the userspace drivers). Its threads use it. A fork
  child shares it with its parent, reference counted; the two shared the
  global tables before, and `forkpipe` (parent reads what the child writes)
  depends on that. The space is released, with everything in it, when the last
  process of the group is reaped.
- Which task is in which space is one `AtomicU16` per scheduler slot
  (`kernel/src/fdspace.rs`), read from the scheduler's lock-free BSP mirror. A
  slot that is handed out again starts in `ROOT` (`sched::alloc_slot`); a
  spawner sets the space before the task can run (interrupts are off from
  spawn to registration since BUG-024).
- The containers are the crate `crates/eurofdspace` (no_std, 8 host tests):
  `Tab` (a table per space, `ROOT`'s inside the static, the others as heap
  blocks), `Lists` (pipes, flags, waiters; empty lists cost nothing), `Guard`
  (a held lock that dereferences to one space), `FlagSlots`, `Registry`.
- **The locks are the same locks**, taken where and how they were: `OPEN_FDS`
  is still one `eurolock` mutex with its site, `PIPE_FDS` and `PIPES` are still
  taken through `pipe_fds()` / `pipes_lock()` with interrupts off. What changed
  is what a guard dereferences to. The 60-odd call sites did not change; `[lk]`
  reports 0 order inversions in every boot.
- Pipe ids count per space. The close-on-exec list is keyed per space.
- A start that finds no heap for the tables (36 kB) or no free space number
  (254) is refused like one that finds no frames, and gives back what it took.
- A task that still points at a released space (a thread that outlived its
  process) is moved to an orphan space, which is emptied for it; it can never
  read or write the launcher's descriptors.

**After** (`ff8d562`, 1 GiB, TCG):

```
[fdspace] 2 background processes keep a pipe and a file open across a foreground launch (musl exit=0, glibc exit=42): phase1=true ended=true pass=2/2 | pid 0x40601: phase2 pipe=ok (write=2 read=2) file=ok (lseek=0 read=1) | pid 0x40602: phase2 pipe=ok (write=2 read=2) file=ok (lseek=0 read=1) | fd spaces in use 4 → 4 → OK ✓
```

Both processes now have fds 3, 4 and 5. OK in all 10 TCG matrix boots and 100
R630 boots of this proof.

**What this round did not do** (the design for the next step):

- The glibc group is one space. A glibc process and its fork children still
  share tables, with the overlays that make that look like separate tables
  (`FD_ALIAS`, `FORK_INHERITED`, `DEFERRED_CLOSE`, `CHILD_OPENED`,
  `FORK_CHILD_CLOSED`, the swap of `ChildMem` at syscall entry). The step
  after this one: a fork child gets a *copy* of its parent's space at fork
  (`Tab::block` from the parent's table), pipes get reference counts per end
  so both tables can name one pipe, and the five overlays go. That is a
  change to every `current_is_fork_child()` branch of the fd syscalls
  (dup2, close, pipe2, open, the close-on-exec walk) and has to be proven
  against chrome's multi-process start, which is why it is not in this round.
- Only one glibc process can be scheduled at a time (`GLIBC_MAIN_TASK` is one
  global); two at once would need their own spaces and their own
  `GLIBC_*` state first.
- epoll instances, eventfds and sockets are numbered in global ranges (500+);
  a background process's sockets are released by pid, not by space.
- `FILES` (the file table the fd tables index) is global by design.

## 2. A failed process start gives back what it took (BUG-053, BUG-061)

**Read, then provoked.** `[bug053]` drains the frame allocator down to the
arena of the process to start and hands back one frame before every further
attempt, for a background musl process, a foreground program and a glibc
process; then it starts a glibc process whose executable is not an ELF. Before
the fix (`045b856`), 1 GiB, TCG (`before-fix-selftests.txt`):

```
[vdso] mapping FAILED — the auxv promised one; expect a crash
[bug053] … glibc refused x3, ran with 3 spare (exit 139), bad executable refused=true kept 49152 frames, next launch exit 42 · frames kept by refused starts: 49152 · free frames 105712 → 56560 → FAILED
```

Two things: the launch with a bad executable kept its 192 MiB arena
(BUG-053), and a glibc process with page tables for its arena but none for
its vDSO was started anyway and died with SIGSEGV (BUG-061; found by this
test, not read before).

**Fix.** `glibc_launch_abort` / `glibc_disk_launch_abort` give back the demand
pool, the address space with the vDSO's table chain, and the arena; every exit
after the arena allocation in `run_glibc`, `spawn_glibc_persistent` and
`glibc_disk_launch` goes through one of them (bad executable or loader, disk
segment map, no kernel stack, scheduler table full, no vDSO tables). The disk
launch also restores the two demand-paging flags on every exit (it left them
on, including when it found no arena). `vdso_prepare` frees a half-allocated
image. The four boot-time `expect`s (`daemon-arena`, `daemon-kstack`,
`utask-arena`, `utask kernel-stack`) are a log line and a return.

**After:**

```
[bug053] process starts with only their arena + k frames free: bg-musl refused x8, started with 8 spare · foreground refused x4, ran with 4 spare (exit 0) · glibc refused x6, ran with 6 spare (exit 42), bad executable refused=true kept 0 frames, next launch exit 42 · frames kept by refused starts: 0 · free frames 105707 → 105707 → OK ✓
```

A background process is refused eight times (four frames of kernel stack,
four of page tables), a foreground program four times, a glibc process six
times (three for the arena's tables, three for the vDSO's); each refusal
leaves the allocator as it found it, the first attempt with enough frames runs
with nothing else free, and the next ordinary launch exits 42. This is also
the test `integ-2026-11` listed as missing ("no test starts a process with
zero free frames"). The glibc part needs a glibc arena: it prints "skipped" at
512 MiB under TCG, runs at 1 GiB and on the R630.

Not provoked, read only: the disk-served launch (`glibc_disk_launch` needs a
EuroPack), "no kernel stack" and "scheduler table full" in the three glibc
launchers.

## 3. BUG-063, found on the way: a stale foreground exit flag

The first version of `[fdspace]` ran its foreground program and then waited
with interrupts on. Under TCG with `net.driver=user` the boot died there, 3
boots of 3 (`bug063-tcg-crash.txt`): the launcher's locals were garbage right
after the program returned, then `GENERAL PROTECTION FAULT … @ timer_switch+0x3e`
(the `iretq`), the frame on the boot task's stack made of boot-stack words.

A foreground program leaves through `sys_exit`: `EXITED = 1`, and
`syscall_entry` returns to the launcher through `SAVED_KERNEL_RSP`. The flag
stayed 1 until the next launch. `syscall_entry` tests it after every syscall
of every task; `daemon_dispatch` and `bg_dispatch` clear it first thing for
that reason, but `udrv::syscall` (the userspace drivers) and the background
routes that return before `bg_dispatch` (nanosleep, fork, wait4, socket calls,
the two Euro sleeps) do not. The next syscall of `e1000d` after a foreground
program had ended took the foreground exit path in the driver's task: six
pops and a `ret` through a stale stack pointer, on the boot task's stack. The
BUG-040 class through another door. It had stayed shut because the boot's own
foreground runs leave interrupts off until the next launch (which clears the
flag). The desktop terminal's `run_args` call restores them, so by the code a
foreground program started there while a userspace driver polls is exposed;
that was not tried from the desktop (the serial console the load tests use
cannot start a program by name, and the load tests start glibc and background
processes, not foreground musl programs). Not seen in a guest before this
round.

Fix (`0009b95`): the exit epilogue and `force_kernel_return` clear the flag.
It is set and consumed inside one syscall, with interrupts off. `[bug063]`
checks the flag after a foreground program and then waits 30 ticks with
interrupts on: OK in all 10 TCG matrix boots and 100 R630 boots.

## 4. BUG-031: not reproduced

`megaraid-proof.sh` with `NO_IOMMU=1` under TCG, the configuration of the one
silent boot (2026-11-05, 1 in 15), on the head of `feature/app-control`
(`7858b72`, with the BUG-032 and BUG-037 fixes), three runs at a time next to
the other team's builds and boots (host load 6 to 21 on 16 cores):
**60 boots, 60 reached the status line, no NMI probe, no fault line**
(`bug031-megaraid-noiommu-tcg-60-boots.txt`). At the old rate the chance of
60 clean boots is 1.6 %. Closed as not reproducible after BUG-037. BUG-063 is
a third thing that would have looked like this (a userspace driver, a wild
return, no fault line); there is still no probe of the original boot, so which
of the three it was stays unknown.

## 5. The lock screen's background (BUG-056)

**Why it got 1.65 times slower without a change.** The settings merge added a
fourth call site of `Color::lerp` (`compositor.rs`), and at `opt-level = "s"`
LLVM stopped inlining it. The link maps show it: in the kernels built before
that merge whose maps are still on the build server (bug-gpf, mem-hygiene)
`fill_rounded_rect_grad` is 1481 bytes and there is no `lerp` symbol; in the
integration's kernel and in the base head it is 951 bytes and `Color::lerp` is
a function of 454 bytes. Out of line, `lerp` makes 26 soft-float calls per
call (disassembly of the base kernel: 6 × `__floatunsisf`, 3 × `__subsf3`,
3 × `__mulsf3`, 3 × `__addsf3`, 3 × `__fixunssfsi`, and 8 compares for the
clamp and the saturating casts); nine of them, the six conversions and three
subtractions of the two end colours, do not depend on the pixel and can be
hoisted out of the loop only when the function is inlined. Measured, not only
read: the base tree with `#[inline(always)]` on `lerp`, booted next to the
unchanged base under the same host load (`lockscreen-timing.txt`):

| Self-test image, TCG, smp 4, 512 MiB, 1920x1080 | `[boot-time] phase lockscreen` |
|---|---|
| base `7858b72` | 88.1 s (88.7 s in a second boot) |
| base + `#[inline(always)]` on `Color::lerp` (not committed) | 52.4 s |
| fast-boot (`proof/fast-boot-2026-11/`) | 50 s |
| this branch | **5.4 s** |

**Fix.** The two colours are five, seven and ten steps apart, so a row of the
gradient is about a dozen runs of one colour. Every channel is monotone along
a row (each float operation of the expression is monotone in the column), so
`lockscreen::paint_background` finds the end of a run by bisection with the
same expression and fills the run with one row fill. About a hundred
evaluations per row instead of 1920, no `put_pixel`.

**The picture is the same.** `[ag-lock]` compares eight rows of the painted
frame (15 360 pixels above and below the card) with the per-pixel expression
in every boot: `background 0 of 15360 sampled pixels off the per-pixel
gradient`. Screenshots of the whole frame, base against this branch, taken
with `boot-time.py --marker "[ag-lock] GUI" --shot` and a fixed RTC
(`screenshots/`, `lockscreen-pixel-compare.txt`):

- KVM (R630): `lock-base-kvm.png` against `lock-kdebug-kvm.png`: **0 differing pixels of 2 073 600**.
- TCG (build server): `lock-base-tcg.png` against `lock-kdebug-tcg.png`: **0 of 2 073 600**.
- A KVM screenshot taken 0.3 s later (`lock-kdebug-kvm-next-frame.png`) already shows this branch's next
  frame, the sign-in frame with an empty field: 122 pixels differ from the base's frame, all inside the
  password field (six masked characters against none). The KVM frames were looked at.

| Self-test image, 1920x1080 | base `7858b72` | this branch |
|---|---|---|
| TCG (build server), `phase lockscreen` (two frames + 0.6 s grace) | 88.7 s | **5.4 s** |
| TCG, one frame (`[ag-lock] … frame`) | about 40 s | **2.6 s** |
| TCG, kernel entry to desktop, `user-smp4-noiommu` | 160.6 s | **71.0 s** |
| KVM (R630, 2 vCPU), `phase lockscreen` | 5.39 s | **0.87 s** |
| KVM, one frame | about 2.4 s | **0.18 s** |

`FrameBuffer::fill_rounded_rect_grad` and `Color::lerp` are not changed (the
icon tiles use them; `graphics.rs` and `compositor.rs` belong to the desktop
work). BUG-057 (the repaint through `fill_rounded_rect`) is the same soft-float
cost elsewhere; neither it nor BUG-054 is touched.

## 6. Reproducible builds (BUG-062)

Two clean builds of `7858b72` in one checkout, 4 minutes apart:
`eurokernel.efi` 5a482cb1… and a1c30093…, **4 bytes different**
(`repro-binary-diff.txt`): file offsets 0x80 and 0x81, the COFF
`TimeDateStamp` of the PE header (0x6af578f0 and 0x6af579e8, the two link
times, 248 s apart), and the same two bytes at 0x406ae44, the `TimeDateStamp`
of the debug directory. Nothing else: no path, no ordering, no build id; the
userland ELFs and their signatures were identical in both builds.

- `.cargo/config.toml`: `-C link-arg=/Brepro` for the UEFI target, so
  lld-link writes a content hash there in every build, `scripts/build.sh`
  included.
- `scripts/repro-build.sh` already passed `/Brepro`, but in `RUSTFLAGS`, and
  `RUSTFLAGS` *replaces* the `rustflags` of `.cargo/config.toml`: the CI job
  "Reproducible build" compared two builds of a kernel without
  `panic=abort`, frame pointers, the link map and the three forced soft crypto
  back ends, not of the kernel `build.sh` builds. The remap flags go through
  `--config` now (cargo joins the arrays), a `RUSTFLAGS` in the environment is
  refused, and the build version is pinned to the commit date.
- Found while looking for the kernel's symbols: kernel and loader are linked
  with the same `/MAP:target/kernel.map`, the loader last, so that file was the
  loader's map after every build (which is why the release kernel "had to be
  relinked for its symbols", and that relink then showed the two different
  hashes). `build.sh` keeps the kernel's map as `target/eurokernel.map`.

After (`repro-after.txt`):

| Build | `eurokernel.efi` |
|---|---|
| `build.sh release` twice, the second after removing the target's UEFI and release directories (final tree) | 5f0efc15… both, 0 bytes different |
| `build.sh image` twice, the same way (final tree) | eff11364… both |
| the same two pairs on the tree of `dbf8460` | 32df30e4… both, 7dd15212… both |
| the CI procedure: `repro-build.sh`, remove the `.efi`, `touch kernel/src/main.rs`, `repro-build.sh` (tree of `dbf8460`) | 0a639923… both, loader 1b0b4ddd… both: REPRODUCIBLE; 0 `/root/…` paths in it (70 in the `build.sh` kernel) |
| `loader.efi` of the four `build.sh` builds of one tree | identical |

The FAT image `eurokernel.img` is not byte-identical from build to build (mkfs.fat volume id and
directory times); that was not asked and not changed.

Still different from build to build by design: `EUROOS_BUILD_VERSION` is the
UTC date in `build.sh` (the commit date in `repro-build.sh`). Not tested here:
two different machines or two checkout paths (the kernel carries 70 strings
with `/root/.cargo` and `/root/.rustup` paths, which `repro-build.sh` remaps
and `build.sh` does not).

## 7. BUG-064, found by the q35 matrix: a self-test that raced the interrupt

The first CI q35 matrix of this round was 12 of 13: leg `usbaudio`,
`[m43-wire] … 0 packet(s) consumed by the USB DAC → no consumption ✗`
(`q35-matrix-first-round.txt`). Nothing in the audio or xHCI code had changed.
`xhci::usbaudio_play` counts the transfer events it reads from the event ring,
`xhci::poll` reads the same ring from the timer interrupt, and the self-test
runs with interrupts on: whoever reads first has the event. Nine runs of the
leg before the fix (`usbaudio-runs.txt`): this branch 0, 0, 2, 2, 1 packets,
the base 1, 2, 2, 1. The test passed on the base as long as one event was
left for it.

Fix (`ff8d562`): the self-test calls `usbaudio_play` with interrupts off and
reads the ring alone. Three runs of the leg after it: 2 of 2 packets each; the
full matrix on the final image is 13 of 13.

## 8. BUG-065, seen in the R630 boots: the generator is not seeded in some KVM boots

Not fixed. The boot loops count a boot as ok when it reaches the desktop and
runs the smoke commands; looking at the FAILED lines of each boot as well
showed `[3d8]` (EuroEntropy) in 8 of the 100 boots of the final image and in
5 of the 78 of the first round, with `[3d7]` and `[3d3]` (which need random
bytes) failing in the same boots:

```
[3d8] EuroEntropy CSPRNG (…): empty-pool-refused=true, jitter-alone≈167bits→ready=false, seeded(jitter+TPM,from-tpm=false)-ready=false, output-nonzero-distinct=false → FAILED
```

The base image `7858b72` booted on the same CPUs at the same time: **0 of 60**;
the final image beside it: **3 of 60** (`r630-bug065-base-vs-final.txt`).
It is not new either: `../robust-2026-11/r630-kvm-highbar-smp4.markers` has the
same line with 21 bits, never written up.

`entropy::gather_jitter` times two instructions between two RDTSCs;
`estimate_jitter_bits` credits a quarter bit for every sample whose difference
from the one before has non-trivial low four bits; `init` gives up after 64
batches of 512 samples whether the pool is ready or not, and the generator then
stays unseeded for that boot. Over 16 batches the estimate is 322 to 1157 bits
in the 60 base boots (the threshold is 256, so the base is not far from it
either) and 810 to 1278 bits in the TCG matrix boots of this proof and of the
integration; in this branch's image it is the same in most boots and 13 to 225
bits in the ones that fail. The self-test runs long before anything this
branch changed, and the machine code of `gather_jitter` is
byte for byte the same in both images. What differs is where it lies: in the
base image both RDTSCs and the instructions between them are in one 64-byte
line (…39d6 to …39e9), in this branch's image they straddle a line boundary
(…67fc to …680f). That the placement is the cause was not tested (no build
with the function moved back); it is what is left.

For whoever takes it: let `init` gather until the pool is ready (the credit
stays as conservative as it is; a quiet core then costs milliseconds instead
of a generator that refuses all boot), seed again on a later request when it
was not ready, and give the sampling loop work whose timing does not depend
on its alignment. `entropy.rs` and `crates/euroentropy` are outside this
round's area and are security code, so nothing was changed here.

## Build server (QEMU 10.2, TCG)

`tcg-boot.sh` and `tcg-matrix.sh` as in `../integ-2026-11/` (two queues, each
QEMU stopped by its own PID, NMI probe on a silent boot: none needed),
self-test image, `-nic user,model=e1000e`, 120 s after the desktop.

| Runs | Result |
|---|---|
| Host tests, `cargo test` (default members) | 187 suites, 1 312 passed, 0 failed, 1 ignored (186 suites and 1 304 tests before: the `eurofdspace` suite with 8 tests is new) (`hosttests.txt`) |
| `user-smp{4,1}-{iommu,noiommu}`, `kernel-smp{4,1}`, `storage-smp{4,1}-iommu-1g`, `combined-smp{4,1}-iommu-1g` | 10 of 10 to the desktop, no fault line, no NMI probe. Per marker against `../integ-2026-11/tcg-*.markers` (`comparison-tcg.txt`): **0 worse, 0 changed** in all ten. New: `[bug063]`, `[fdspace]`, `[bug053]`, OK in all ten (the glibc part of `[bug053]` ran in the four 1 GiB boots and is skipped in the six at 512 MiB). Kernel entry to desktop 67 to 79 s at 512 MiB (integration: 144 to 159 s) and 221 to 306 s at 1 GiB (296 to 380 s); `lockscreen` 5.0 to 5.9 s (94 s) |
| `[lk]` | `boot-total order inversions=0 (self-test provoked=2) task-lock-with-IF0=0` in all ten (`tcg-summary.txt`) |
| CI q35 matrix, `run-metal-matrix.py --legs base,nvme,ahci,e1000e,hda,usb,usbhub,usbaudio,usbnet,scan,printer,power,hwprobe` | 13/13 PASS on the final image; 12/13 in the first round (`usbaudio`, BUG-064, `q35-matrix-first-round.txt`) (`q35-matrix.txt`) |
| `megaraid-proof.sh` with IOMMU, and with `NO_IOMMU=1` (4 boots, final image) | 0 markers worse than `../integ-2026-11/` (`comparison-mr.txt`); host checks 4/4 in both |
| `megaraid-proof.sh`, `NO_IOMMU=1`, base head, 30 runs (BUG-031) | 60 of 60 boots to the status line |

The first round, on the tree of `dbf8460`, gave the same matrix verdicts (10 of
10, 0 worse in all ten, `[lk]` 0 inversions); its q35 matrix was the 12/13.

### Release image (`./scripts/build.sh image`)

| Test | Result |
|---|---|
| `wizard-install-test.py` (TCG) | 14/14 on the final release image (`wizard-install-test.txt`) |
| No self-test of this round in the release image | 0 lines of `[bug063]`, `[fdspace]`, `[bug053]`, `[ag-lock]` in the boot log of the final release image |

## Dell R630 (KVM)

Only built images went there, to `/srv/euroos/kdebug-runs`; the harness
scripts were copied on the R630 from the integration proof's copy
(`loadtest.py`, `loadtest-http-peer.py`, `boot-time.py`, `markers-compare.py`:
same sha256 as this tree's; `analyze_soak.py`, the integration's helper, is in
this directory). No source tree, no key.
Every QEMU with SLIRP `restrict=on`, pinned, no VFIO: no PCI device was
touched. The machine was shared with the desktop team's runs the whole time,
so the CPUs were not always ours alone.

Two rounds. The first ran on the image of `dbf8460` (sha256 f02e7660…), was
45 minutes into its soak and 78 boots into its 100 when the q35 matrix showed
BUG-064, and was left to finish its soak; the second ran everything again on
the final image.

### 100 short boots (`r630-boots-summary.txt`)

`loadtest.py --kvm --smp 4 --mem 4G --nic e1000e --restrict --nmi-on-hang
--scenario smoke`, each to the desktop and through the smoke commands:

| Configuration | Boots | Result |
|---|---|---|
| user driver (default), smp 4 | 40 | 40 ok |
| user driver + `intel-iommu`, smp 4 | 30 | 30 ok |
| user driver, smp 1 | 30 | 30 ok |

**100 of 100** to the desktop and clean through the smoke commands; none silent, no fault line, no NMI
probe. In each of the 100: `[bug063]`, `[fdspace]` and `[bug053]` OK, `[lk] boot-total order inversions=0`,
`[ag-lock] … background 0 of 15360 sampled pixels off`; one lock-screen frame takes 180 to 310 ms (median
240), the `lockscreen` phase 0.86 to 1.07 s. First round: 78 of 78 (26 per configuration) when it was
stopped (`r630-boots-first-round-summary.txt`).

FAILED lines in these boots: the five that a guest without a route out always has (`[udrv-net]`,
`[udrv-crash]`, `[udrv-linux]`, `[conn]`, `[fuzz]`, as in the integration proof), and in 8 of the 100
`[3d8]`, with `[3d7]` and `[3d3]` in 4 of those: BUG-065, section 8.

### Soak, 2 h 05

`loadtest.py --kvm --smp 4 --nic e1000e --restrict --nmi-on-hang --mem 16G
--disks 8G --host-http --hostfwd 18581 --hammer 4 --procs 32 --scenario soak
--soak-churn-proc 8 --duration 7500`, self-test image, `net.driver=user`: the
configuration of `../integ-2026-11/`.

| | first round (image of `dbf8460`, `r630-soak-first-round*`) | final image (`r630-soak*`) |
|---|---|---|
| commands | 2 724, all ok, `completed clean` | 2 671, all ok, `completed clean` |
| `churn proc` commands / glibc launches (`GLIBC-DYNAMIC-OK`) | 348 / 2 808, every `[churn-proc]` verdict OK | 342 / 2 758, every `[churn-proc]` verdict OK |
| `heap_used` first / at 10 min / last | 149 649 488 / 148 502 688 / 148 525 216 | 149 649 744 / 148 500 384 / 148 522 656 |
| slope after 10 min (least squares) | **+0.17 kB/min** (range 1 282 kB) | **+0.17 kB/min** (range 1 282 kB) |
| tasks / files / kernel stacks / process pool | 56 / 187 / 0 / 887 296, flat | 56 / 187 / 0 / 887 296, flat |
| free frames | 2 964 754 -> 2 981 394 (the first wave's zombies reaped) | 2 964 756 -> 2 981 396 |
| httpd during the soak | 910 221 answered, 11 failed, p50 5.1 ms, p99 124 ms | 883 286 answered, 10 failed, p50 5.3 ms, p99 124 ms |
| serial | 0 panics, 0 fault lines, 0 NMI probes, 0 watchdog lines, 0 refused frame frees | the same: 0 of each |
| boot of the guest against the integration's soak boot (`comparison-r630-soak-boot.txt`) | 0 worse; new `[bug063]`, `[fdspace]`, `[bug053]`, OK | 0 worse; the same three, OK |

Integration: +0.25 kB/min, range 105 kB, 1 006 005 answered. The range is 1 282 kB now because every
wave of 32 workers holds 32 x 38 kB of descriptor tables until it is reaped; the level between the waves
is what the slope is drawn through, and it is flat. The guest's boot took 361 s to the desktop (367 s in
the first round) with the three new self-tests in it (`[bug053]` drains 16 GiB three times; such a guest
took 330 to 455 s before). The four `no frames for page tables` lines and the one `not a loadable ELF`
line in each soak log are `[bug053]` at boot. The R630 was shared this time, which is where the 10 % fewer httpd answers are
from; nothing failed more.

### Boot time of the release image (`kvm-5x.txt`)

`boot-time.py`, `-smp 2 -m 2G`, `restrict=on`, host time from QEMU start to
`[desktop] interactive loop started`:

| Configuration | base `7858b72`, median (range) | final image, median (range) | 2026-11-10 (`182b19c`) |
|---|---|---|---|
| SATA boot medium + legacy virtio-net | 2.69 s (2.53 to 3.06) | **2.71 s** (2.39 to 3.03) | 2.71 s (2.42 to 2.92) |
| modern virtio only | 2.63 s (2.58 to 3.07) | **2.50 s** (2.41 to 2.71) | 2.34 s (2.05 to 2.43) |

Five boots each, base and final image alternating on the same two CPUs (`r630-boot-time-ab.sh`), so that
both see the same host. Kernel entry to desktop by `[boot-time]`: base 839 to 897 ms, final image 790 to
894 ms (2026-11-10, with the machine to itself: 790 to 828 ms). An earlier pair during the boot loops,
with the host busier, gave 2.86 s for the base and 2.90 s for the first-round image
(`kvm-5x-first-round.txt`).

## Open

- **BUG-065** (section 8): in 8 of the 100 R630 boots of the final image the entropy self-test `[3d8]`
  FAILED and in 4 of them the generator stayed unseeded; the base image booted beside it: 0 of 60. Not
  fixed, not in this round's area. This is the one thing in the proof that is worse than the base.
- **Fd spaces, next step** (section 1): the glibc group is still one space with the per-child overlays;
  epoll, eventfd and socket descriptors are numbered globally. If the heap runs out while the very first
  background process is made, the orphan space can be left with only part of its tables (it is then
  completed by no one; a task moved there would fall back to the launcher's tables for the missing ones).
- **BUG-053**: the disk-served launch and the `(no kernel stack)` / `(scheduler task table full)` exits were
  read, not provoked. `[bug053]` raises the frame allocator's high-water mark to all of RAM in the self-test
  image (a diagnostic number only).
- **BUG-031**: closed without a probe of the original boot; three bugs fit it.
- **BUG-056**: `Color::lerp` is still out of line and `fill_rounded_rect_grad` still per-pixel float; BUG-054 and
  BUG-057 pay the same cost. That is `graphics.rs` / `compositor.rs`.
- **BUG-062**: two machines or two checkout paths were not compared; the FAT image is not byte-identical.
- **BUG-063**: the exposure from the desktop terminal was read from the code, not tried there. The other
  globals of a foreground run (`SAVED_KERNEL_RSP`, `EXIT_CODE`, `OUTPUT`) are as they were.
- **BUG-064**: only the self-test changed; `usbaudio_play` still shares the event ring with `xhci::poll`
  when it is called with interrupts on (it has no such caller left).
- **Release image boot time**: 2.71 s, the base image beside it 2.69 s. With modern virtio only it is 2.50 s
  against 2.34 s on 2026-11-10, and so is the base (2.63 s): the R630 was shared this time.
- BUG-052, 054, 055, 057, 058: not in this round.

## Cleaned up

- Build server: `target/` of the worktree, every built image and kernel in `/opt/euroos/kdebug-runs`, the
  QEMU work files, the throwaway worktree `/opt/euroos/wt-kdebug-x` (base tree and `#[inline(always)]`
  experiment) and the symlink to the dev key are removed; the serial logs of the final runs and the patch
  scripts stay in `/opt/euroos/kdebug-runs`. The main checkout was read only.
- R630: images, disk files and work directories under `/srv/euroos/kdebug-runs` are removed; the run
  outputs stay there. No VFIO binding was made, 02:00.x is on `tg3` as it was.
- One thing that should not have happened on the R630: a wrong process walk in a clean-up command sent
  SIGTERM to PID 1 (twice). systemd answers that with a re-exec of itself; the system stayed `running`,
  no unit failed, no service restarted (the man-db, apt-daily and motd timers ran at that moment).
