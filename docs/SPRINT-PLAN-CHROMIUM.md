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
      **ATTEMPT 1 (2026-07-18, reverted — saved as docs/wip/per-task-syscall-reentrancy.patch):**
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
