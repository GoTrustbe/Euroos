# Sprint Plan: Chromium rendering on EuroOS

Status baseline (2026-07-17): the real 485 MB `chrome` binary runs from a disk-served,
demand-paged loader — `chrome --version` → `Chromium 152.0.7952.0`, exit 0. `chrome
--headless --dump-dom` walks through PartitionAlloc, the user-data-dir, and socket
setup, then stops at `fork()` (crashpad spawns a child crash-handler process).

Goal: a Chromium that renders a real page — first `--dump-dom` (Blink builds a DOM),
then a headless screenshot (Blink paints), then an interactive window on the desktop.

Work top-to-bottom. Commit after each green step. Each `[ ]` is a boot-verified step.

## Phase A — get past crashpad to a rendered DOM (cheapest path first)
- [ ] A1. Skip the crashpad handler so `--headless --dump-dom` proceeds without fork.
      Try, in order: env `CHROME_HEADLESS_NO_CRASH`, switches `--disable-crashpad`,
      `--disable-features=Crashpad`, `--crash-dumps-dir` tricks, and building the arg
      set that makes `crash_reporter::InitializeCrashpad` a no-op. If a switch works →
      chrome should reach Blink and dump the DOM.
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
