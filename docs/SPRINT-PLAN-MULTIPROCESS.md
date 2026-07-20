# Sprint Plan: Multi-process support (the real path to the Chromium DOM)

## Why
`chrome-headless-shell --single-process` runs its FULL init on EuroOS (all kernel
bugs in that path fixed, commits up to 77a6b91) but never emits a DOM: worker
threads deterministically hit `IMMEDIATE_CRASH` (`int3;ud2`) on failed CHECKs.
Web-confirmed: `--single-process` (new) headless chrome is upstream-broken and
crashes worker threads **on real Linux too** (puppeteer #2512/#5258/#10265,
chromedp #1000). So the DOM requires MULTI-PROCESS chrome (its default mode).

Chrome spawns children via **plain `fork()` + `execve()`**: measured
`clone(flags=0x1200011, stack=0)` = `CLONE_CHILD_SETTID|CLONE_CHILD_CLEARTID|SIGCHLD`
(no CLONE_VM/VFORK/THREAD) then `execve(/pack/chrome-headless-shell, --type=...)`.
Today the glibc dispatcher ENOSYSes both. `do_fork`/`do_execve` exist ONLY for the
musl/bg 2 MB-arena model, not the demand-paged glibc model.

## The hard part
The glibc process has TWO memory regions:
1. **Arena** (identity-mapped, ~96 MiB): ld.so + libc + heap + stack.
2. **Demand region** (`DEMAND_BASE`=1 TiB, 256 GiB sparse): the disk-served exe
   (read-only, demand-paged from disk) + ld.so-mmap'd libraries + anon mmaps.
   Descriptors live in `DEMAND_FILE_MAPS`; committed frames from `DEMAND_POOL`.
All chrome threads currently SHARE one `GLIBC_PML4`. Multi-process needs a
SEPARATE address space (PML4) per process.

## Milestones (each independently verifiable)

### M1 — `fork()` for the demand-paged model  [START HERE]
- New child PML4 (copy kernel half, fresh user half).
- Arena: COW — mark parent+child arena PTEs read-only, share frames, refcount;
  on write-fault copy the page. (Eager full copy is simpler but ~96 MiB/fork ×
  several forks blows the budget; COW is required.)
- Demand region: share the disk-backed descriptors (read-only exe/libs re-fault
  from disk independently); COW the committed ANON/heap demand pages.
- Per-process state must move OUT of globals into a per-process struct: the
  single `GLIBC_PML4`, `DEMAND_NEXT`, `DEMAND_FILE_MAPS`, `DEMAND_POOL`, `FILES`
  fd table, `HEAP_BREAK`, `CURRENT_CAPS`. This is the big refactor — today they
  are one-process globals.
- Child task resumes at the fork-return RIP with rax=0, its own PML4.
- **Verify:** a SMALL glibc program that `fork()`s, both branches print, parent
  `wait4`s the child. (Need a glibc forktest.elf — current forktest is musl/bg.)

#### M1 concrete recipe (derived + partly built this session)
DONE (verified): `handle_demand_fault` maps into the CURRENT CR3 (commit 71b757f).
DONE (compiles, not yet wired): `paging::fill_remap_tables_multiblock` (child PML4
remapping the multi-block arena to child frames, kernel identity preserved) +
`paging::clone_demand_region(parent, child, idx)` (copies every committed demand
page into fresh pool frames at the same VA in the child).

`do_glibc_fork()` (to write, wire into clone(56, no CLONE_VM)/fork(57)):
1. Allocate the child arena PAGE-BY-PAGE from the DEMAND pool (procpool::demand_alloc)
   — the process pool (procpool::alloc/alloc_contiguous) is too small (the demand
   pool took ~all RAM), so a contiguous 96 MiB huge-page arena isn't available.
   => write a 4 KiB-page variant of fill_remap_tables_multiblock (map each arena page
   via map_demand_4k) instead of the 2 MiB-huge version, OR copy arena pages into the
   demand region tables. Copy each parent arena page's bytes into the child frame.
   (Optimization later: skip all-zero pages via a shared zero frame; chrome touches
   maybe 10-20 MiB of the 96 MiB arena.)
2. PML4: fresh frame; set kernel/high entries like fill_remap_tables_*, arena entries
   to child frames.
3. `clone_demand_region(parent_pml4, child_pml4, DEMAND_PML4_IDX)` for the exe/heap.
4. Child task: sched::spawn_thread-style but with the CHILD PML4 (its own cr3), rax=0,
   resume at USER_RIP (fork return). NOT a thread (own address space).
5. FILES fd table: today OPEN_FDS is a single global — the child must get its OWN copy
   (fds inherited then diverge). Snapshot OPEN_FDS into the child; needs per-process fd
   state (or a fork-time clone keyed by task). This is the FILES part of the refactor.
6. Process table entry (pid, child_pml4, exit status) for wait4/SIGCHLD (M3).
7. Return child pid to parent; child returns 0.
GOTCHA: DEMAND_NEXT/DEMAND_FILE_MAPS are global — OK for the fork window (child faults
EXISTING mappings into its own CR3) but execve (M2) must not clobber the parent's; the
shared-descriptor analysis (each mapping at a unique VA via the shared bump) may let
them stay global if execve only ADDS the child's new VAs. Verify empirically.
TEST: chrome WITHOUT --single-process → [spawndiag] should show fork SUCCEED (child
task spawned) then the child's execve(--type=renderer) attempt.

### M2 — `execve()` for the demand-paged model
- Replace the child image: tear down its arena+demand, re-run the
  `run_glibc_disk` loader path (load ld.so + disk exe, build argv/envp/auxv),
  jump to ld.so entry — all in the EXISTING task/PML4.
- **Verify:** a glibc program that `fork()`+`execve("/bin/gtiny")`; gtiny runs.

### M3 — process table + `wait4`/SIGCHLD + exit reaping
- Track child processes (pid, pml4, exit status). `wait4` blocks until a child
  exits; deliver SIGCHLD (chrome waits on children).
- **Verify:** chrome (no `--single-process`) forks the GPU/renderer without ENOSYS.

### M4 — cross-process shared memory
- Chrome shares `memfd`/shm between parent and child (Mojo buffers, the
  compositor frame). A child inherits fds across fork; a shared memfd must map to
  the SAME physical frames in BOTH address spaces. Today `memfd` is per-process
  FILES-backed; make shared-mapping frames refcounted and mapped into each PML4.
- **Verify:** parent writes a memfd, child (post-exec, via inherited fd) reads it.

### M5 — Mojo IPC across processes
- Chrome's Mojo channel is a `socketpair` inherited across fork; the parent keeps
  one end, the child the other. Our AF_UNIX socketpair must survive fork (fd
  inheritance) and deliver bytes cross-process. Plus fd-passing via SCM_RIGHTS
  (Mojo passes handles over the channel).
- **Verify:** browser<->renderer Mojo handshake completes (renderer logs appear).

### M6 — GPU process / compositor
- The GPU process (child) runs Viz with a software SkiaRenderer (no GL; SwiftShader
  needs AVX2 which qemu64 lacks — keep `--use-gl=disabled`, software compositing).
- **Verify:** `VizNullHypothesis` + compositor init COMPLETES in the GPU child.

### M7 — navigation + DOM
- With browser + renderer + GPU processes up, `--dump-dom file:///tmp/euro.html`
  navigates and serializes `<h1>EuroOS</h1>`.
- **Milestone: Blink runs, DOM round-trips.**

## Reality check
M1-M3 (fork/exec/wait) is a bounded kernel project (the per-process-state refactor
is the bulk). M4-M6 each carry their own risk; chrome multi-process assumes a lot
of Linux (shared shm, SCM_RIGHTS fd-passing, process lifecycle). This is
multi-week. The alternative reachable real browser remains WebKitGTK (single
address space, no multiprocess), per earlier analysis.

## Test harness (unchanged)
Build `./scripts/build.sh release`; boot foreground (host has ~2.4 GiB free →
`-m 1536M`..`1900M`), qemu MUST be `-serial stdio | tee` (background re-sandboxes;
`-serial file:` is killed as a no-output hang). Pack `/tmp/hs-pack.img`. Each boot
~3-4 min wall (TCG ~60x). `[spawndiag]` logs fork/exec attempts.
