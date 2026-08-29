# Sprint Plan: Chromium rendering on EuroOS

Status baseline (2026-07-17): the real 485 MB `chrome` binary runs from a disk-served,
demand-paged loader — `chrome --version` → `Chromium 152.0.7952.0`, exit 0. `chrome
--headless --dump-dom` walks through PartitionAlloc, the user-data-dir, and socket
setup, then stops at `fork()` (crashpad spawns a child crash-handler process).

Goal: a Chromium that renders a real page — first `--dump-dom` (Blink builds a DOM),
then a headless screenshot (Blink paints), then an interactive window on the desktop.

Work top-to-bottom. Commit after each green step. Each `[ ]` is a boot-verified step.

## Phase A — get past crashpad to a rendered DOM (cheapest path first)
- [x] A1. crashpad skipped via `--disable-crashpad-for-testing` (2026-07-17). Then a
      LONG chain of syscall/VFS blockers, each named by chrome, each fixed + committed:
      pipe/pipe2 (22/293) + CAP_NET · AF_UNIX socket/bind/listen/accept/unlink ·
      symlink/readlink · stat(4)/lstat(6) + dirs report 0700 · access() disk-aware ·
      VFS dir-awareness for disk files (locale bundle) · epoll (create/ctl/wait) ·
      MAX_TASKS 48→256, MAX_THREADS 64→224, clone→-EAGAIN (no panic) · blocking pipe
      reads + fcntl O_NONBLOCK · vfs_pread EOF-slice panic fix · epoll_wait yields.
      chrome --headless now runs DEEP into browser init: PartitionAlloc, ProcessSingleton,
      ResourceBundle (en-US locale), ~30 threads, dbus/inotify probes (non-fatal).
      **BLOCKED (root-caused 2026-07-18): a busy-spin LIVELOCK, not a deadlock.**
      Diagnostic snapshots (gate: ring3::STALL_DIAG): +38952 futex_wait + ~13000
      epoll_wait per 700 ticks, 7 Blocked / 10 Ready, zero forward progress.
      ROOT CAUSE: the syscall handler runs on ONE shared KERNEL_RSP stack, and
      USER_RSP/USER_RIP/SAVED_REGS are globals (ring3.rs ~L201) set on entry + used
      on exit. A glibc thread therefore CANNOT deschedule mid-syscall (a yield lets
      another thread's syscall clobber the shared stack + ring3-return state), so
      futex_wait can only mark Blocked + return, and the thread busy-spins re-checking
      until the timer preempts it — fine at few threads (gsync/gthread pass), fatal at
      chrome's ~30-thread contention.
      **DONE + committed (2026-07-19, commit `40e8822`): per-task syscall reentrancy
      WORKS and is SAFE.** CURRENT_SC_STACK per-task (schedule_core points it at the
      incoming task's kstack; syscall_entry uses it not KERNEL_RSP) + USER_RSP/USER_RIP/
      SAVED_REGS as per-task Task fields saved/restored on switch + futex_wait/epoll_wait
      yield after block_current — GATED by SYSCALL_YIELD_OK (true in linux_dispatch, false
      in bg_dispatch, because the musl/DOOM bg path holds BG.lock across the syscall so a
      mid-syscall yield there wedges on BG.lock; attempt-1 hung the musl thread test for
      exactly this). Verified: musl fork+clone+pthread_join, glibc gthread + gsync(mutex/
      condvar), LINUX COMPAT 20/20 all pass. glibc futex threads now truly block (0 cpu)
      instead of the +38952 futex_wait/700t busy-spin.
      **BUT chrome --headless STILL wedges at ~30 threads (task 39), and now HARDER: the
      diagnostic snapshot can no longer fire — even the launcher task 0 can't get CPU,
      and the iteration-based snapshot (commit e16ea57, gated STALL_DIAG) never dumps →
      a scheduling wedge (likely a dead timer / IF=0 spin, or all-Blocked + a lost wake
      that also starves task 0's fallback). This is a DEEPER scheduler-scale issue than
      the futex livelock.** NARROWED (commit 8df6eb0): schedule_core logging shows the
      scheduler is NOT called after task ~39 AND no sched-guard skip message → the CPU is
      in a tight IF=0 spin that never yields nor takes a timer (timer dead) = a spinlock/
      self-deadlock, NOT a scheduling bug and NOT the futex livelock. Distinct from the
      pre-reentrancy userspace busy-spin (which kept the timer alive). PRIME SUSPECT: a
      syscall holds a spin::Mutex (FILES/DISK_FILES/DEMAND_FILE_MAPS/OPEN_FDS) then touches
      DEMAND memory → the #PF demand-fault handler (handle_demand_fault, ring3.rs ~L4423,
      takes DEMAND_FILE_MAPS+DISK_FILES+FILES) wants the same lock → self-deadlock, IF=0,
      timer dead. e.g. vfs_read/vfs_pread hold FILES.lock while copy_nonoverlapping'ing to
      a user buffer that lives in the demand region → fault mid-copy. NEXT: (a) confirm —
      make handle_demand_fault try_lock those + log on contention; (b) fix — never hold
      FILES/etc across a user-memory copy that can fault (clone bytes, drop lock, then
      copy); audit every vfs_* + demand-mmap path. A lock-ordering bug exposed at chrome's
      concurrent-demand-fault scale.
      UPDATE (commit 2fd493d): FIXED the vfs_read/vfs_pread instance (clone-then-copy,
      drop FILES.lock before the user copy) — verified safe (all glibc read tests +
      crashpad + chrome --version pass) — BUT chrome --headless STILL wedges at task ~39.
      So that FILES self-deadlock was real but NOT the chrome wedge. The wedge remains a
      tight IF=0 spin (scheduler never re-entered, timer dead) in chrome's multithreaded
      startup; it resisted scheduler-level instrumentation. NEXT: capture the RIP of the
      spin — a watchdog/NMI (or a 2nd CPU via SMP) that dumps the current instruction
      pointer when ticks stall for N wall-ms — to name the exact spinning function, then
      fix that lock/loop. All else (futex livelock, per-task syscall reentrancy, FILES
      deadlock) is fixed + committed.
      **ATTEMPT 1 (2026-07-18, reverted):**
      implemented per-task syscall stack (CURRENT_SC_STACK global set by schedule_core
      to the incoming task's kstack; syscall_entry uses it not KERNEL_RSP) + saved/
      restored USER_RSP/USER_RIP/SAVED_REGS as Task fields in schedule_core + made
      futex_wait/epoll_wait call sched::yield_now after block_current. Basic single-
      thread syscalls fine, but it HANGS the musl fork+clone+pthread_join test (freezes
      right after `[thread] clone -> task 24`, where the known-good boot continues to
      `GTHREAD ... 3 threads joined -> PASS`). So a descheduled-mid-futex musl joiner
      never resumes (or its worker never runs) — a subtle bug in the yield/resume or
      the per-task-stack handling of a forked child's first run. Reverted to keep the
      working system (chrome --version, GTK, DOOM, 20/20). NEXT debugging: add a log in
      futex_wait (after yield_now returns) + futex_wake/unblock to trace the wake→resume
      of the yielded joiner; check the forked-child first-run path (sc_* = 0 → set_
      syscall_globals) and whether yield_switch fully preserves a mid-syscall frame.
      **THE FIX (P0): per-task syscall reentrancy** — give each thread its own
      syscall kernel stack, make USER_RSP/USER_RIP/SAVED_REGS/KERNEL_RSP per-task
      (fields on Task, saved+restored in the context switch, KERNEL_RSP set to the
      incoming task's syscall-stack top). Then futex_wait/epoll_wait can truly yield
      (sched::yield_now) and a Blocked thread uses ZERO cpu until woken. Touch points:
      syscall_entry asm (ring3.rs ~L2545, uses [rip+KERNEL_RSP]/[rip+USER_RSP]), the
      Task struct + context switch (sched.rs). Delicate — breaks ALL glibc if wrong;
      test incrementally (gtiny first, then gthread/gsync, then chrome). Chrome pack:
      /tmp/chrome-pack.img (550 MiB, 92 files incl locales/en-US.pak). Boot: -m 3072M
      + pack as virtio-blk; --headless needs a long timeout (~60x slow under TCG).
- [ ] A2. If no switch skips it: make crashpad's `fork()` survivable — implement a
      minimal `fork(57)`/`clone(process)` that returns a child PID and a child task in
      a COPIED address space, and `execve(59)` that replaces the child image with the
      served `chrome_crashpad_handler`. Enough for `StartHandler` to return success.
- [ ] A3. Whichever route: capture the actual `--dump-dom` output (the `<h1>EuroOS</h1>`
      round-trips through Blink's HTML parser + DOM serializer). **Milestone: Blink runs.**

## Phase B — real multi-process (fork + exec + wait + IPC)
- [ ] B1. `fork(57)` / `clone(SIGCHLD)`: new PML4, copy (or COW) the parent arena +
      demand region, dup FILES/fd table + open sockets, child returns 0, parent gets pid.
- [ ] B2. `execve(59)`: load a new ELF image (disk-served or embedded) into the child,
      reset the stack/auxv, jump to its ld.so. Inherited fds/sockets survive.
- [ ] B3. `wait4(61)`/`waitpid`: reap a child, deliver its exit status; `SIGCHLD`.
- [ ] B4. Cross-process shared memory: `memfd_create(319)` + `mmap` of a shared fd, so
      two processes map the same physical pages (chrome's `base::SharedMemory`).
- [ ] B5. Inter-process AF_UNIX: a socket created in the parent, passed to the child by
      fd number, carries messages (chrome Mojo/legacy IPC + fd passing via SCM_RIGHTS).
      **Milestone: a chrome renderer subprocess launches and talks to the browser.**

## Phase C — headless rendering (Blink paint → pixels, no GPU)
- [ ] C1. `--headless` with `--single-process` off: browser + renderer processes.
- [ ] C2. Software compositor path (`--disable-gpu`): Blink paints into a bitmap via
      Skia's software rasterizer (no GL). Verify with `--screenshot` → a PNG in the VFS.
- [ ] C3. Fonts: chrome/Skia find our served fonts via fontconfig (reuse the GTK cache).
      **Milestone: a headless screenshot PNG of a rendered page.**

## Phase D — GPU / GL (software first, then real)
- [ ] D1. SwiftShader (chrome's bundled software GL/Vulkan): serve `libvk_swiftshader`,
      `libGLESv2`, `libEGL`; make `--use-gl=angle --use-angle=swiftshader` initialize.
- [ ] D2. Stub the `libgbm`/DRM ioctls SwiftShader needs, or force the pure-software
      raster path so no DRM is required.
      **Milestone: GPU-process init succeeds on software GL.**

## Phase E — sandbox (or a clean no-sandbox path)
- [ ] E1. Keep `--no-sandbox` working end to end (namespaces/seccomp not required).
- [ ] E2. (Stretch) minimal user-namespace + seccomp acceptance so the default sandbox
      path no longer aborts.

## Phase F — interactive browser on the EuroOS desktop
- [ ] F1. Launch full chrome (not headless) as a persistent app; its X11/Ozone window
      maps through our in-kernel X server (reuse the GTK live-window path).
- [ ] F2. Route desktop keyboard/mouse into the chrome window; present its framebuffer.
- [ ] F3. Load a real local page, then `euro-os.eu` over the netstack.
      **Milestone: a real web page visible + interactive in a window on EuroOS.**

## Method
- One boot per verified step; let the binary name each blocker; knock it down; commit.
- No Claude trailers. Do not push. Under-claim: "engine works" ≠ "app works".
- Keep the pack disk (`/tmp/chrome-pack.img`) as the chrome source; grow it as needed.

## 2026-08-28 — deadlock work (phase C1 approach)
Baseline first: the known-good --single-process desktop run on the NEW kernel
(rwx enforcement, immutability, integrity sweep landed since the last chrome
run). Then remove --single-process and hunt the multi-process wall with the
existing forensics: FOP ring (last 128 futex ops), FUTEX_WAIT_ADDR/SINCE per
task, stall snapshots (STALL_DIAG), read_glibc_u32 lock-word peeks. Expected
new ground in multi-process: fork/exec of the renderer (no zygote), Mojo IPC
over socketpairs between REAL processes, cross-process shared memory
(memfd + MAP_SHARED across address spaces).

### 2026-08-28 findings — the "deadlock" dissected
The multi-process wall is NOT one deadlock; it is layers:
1. FIXED: chrome-desktop.sh lacked the -icount guest clock — chrome sat in the
   TCP_INFO socket treadmill and never reached the window (same fix as
   chrome-ui-input.sh, now committed).
2. FIXED: the process frame pool fell back to 160 MiB at -m 3584M (the 1/5-of-RAM
   cap rejected 640 MiB), so not one 256 MiB fork arena fit and every GPU/
   renderer/network child died at birth ("GPU process isn't usable. Goodbye").
   Cap is now 1/4 with 512/288 MiB intermediate candidates: two real chrome
   children forked successfully (own PML4 + arena).
3. OPEN (next iteration): the forked child completes its post-fork rt_sigaction
   sweep over all fds, then sits Ready but syscall-silent — the Mojo handshake
   with the browser never completes (suspects: child-side thread creation after
   fork, socketpair inheritance, or the child waiting on a poll the parent never
   satisfies). The network-service child crashes minutes later; child arenas are
   not recycled into the pool on exit (third fork fails at 127 MiB left).
Baseline re-proven the same day: --single-process chrome renders the test page
on the desktop under the full FS-security kernel, and opened a TLS connection
to 142.251.142.206:443 through the EuroOS netstack.

### 2026-08-29 — three real kernel bugs fixed under the Mojo wall
1. clone/clone3 gave a NEW THREAD the global GLIBC_PML4 — a thread created by a
   forked child ran on the PARENT's memory copy (child and its own thread never
   saw each other's writes). Both arms now use the caller's Cr3.
2. The fd table is process-global (that is what makes fork inheritance work),
   so the child's post-fork fd cleanup CLOSED THE PARENT'S descriptors — the
   "network service crashed" trail. A fork child's close() now only marks the
   fd in a per-child closed set.
3. A fork child's exit fell into the main-process path (ending the whole
   browser) and leaked its 256 MiB arena. Children now recycle arena + page
   tables + kstack into the pool and die quietly.
Result: after the forks the run now spawns 8 further threads and lives longer,
but the child STILL goes syscall-silent right after its first post-fork sweep,
chrome times the launch out (3 tries) and aborts. NEXT ITERATION: a per-child
FULL syscall trace (not just sockets) to see the exact last thing the child
does — the [slife] socket-only log hides everything between the sweep and the
silence. --single-process restored as the shipping default.
