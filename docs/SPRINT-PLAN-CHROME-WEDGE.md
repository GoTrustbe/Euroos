# Sprint Plan: crack the chrome --headless IF=0 wedge, then render

Baseline (2026-07-19): chrome --version works; chrome --headless walks through ~15
blockers into the multithreaded browser core then WEDGES at ~thread 30 (task 39) in a
tight IF=0 spin — the timer dies, the scheduler is never re-entered. It is NOT the
(fixed) futex livelock, NOT a scheduling-logic bug, NOT the (fixed) FILES self-deadlock.
It resisted timer/scheduler-level instrumentation because those depend on the timer,
which is dead. Need to capture the RIP where the CPU spins.

Work top-to-bottom, commit after each green step, no pausing.

## Phase A — capture the spin site (RIP) with an NMI
- [ ] A1. NMI handler that DUMPS: an NMI is delivered even with IF=0, so it fires during
      the wedge. Add/confirm an IDT vector-2 handler that prints the interrupted RIP +
      CS/RFLAGS + a raw stack scan of return addresses to serial. Own IST stack.
- [ ] A2. Boot chrome --headless with QMP (`-qmp unix:.../qmp.sock,server,nowait`); when
      the log freezes at task ~39, `inject-nmi` via QMP. The NMI dump names the RIP.
- [ ] A3. Map the RIP to a function: `nm`/`addr2line` on the kernel ELF (the .efi has
      symbols) — or match against the serial `[panic] anchor` offsets. **Milestone: the
      exact spinning function is named.**

## Phase B — fix the spin
- [ ] B1. If it's a spin::Mutex held across a fault/yield: restructure so the lock is not
      held across a user-memory touch or a yield (clone-then-copy / drop-before-touch),
      mirroring the vfs_read/pread fix. Audit the named function + its callers.
- [ ] B2. If it's an infinite Rust loop (e.g., a poll that never completes because its
      completion needs an IRQ that IF=0 masks): make it bounded / IF-aware / yield.
- [ ] B3. If it's the virtio/disk poll during a demand fault under IF=0: ensure the poll
      path works with interrupts masked (it does for --version; check the concurrent case).
      **Milestone: chrome --headless no longer wedges at task 39.**

## Phase C — reach a rendered DOM
- [ ] C1. Let chrome --headless --dump-dom run to completion; capture the `<h1>EuroOS</h1>`
      round-tripping through Blink's parser + DOM serializer. **Milestone: Blink runs.**
- [ ] C2. Knock down any further named blockers (each fixed + committed) until --dump-dom
      prints the DOM.

## Phase D — headless screenshot (Blink paints pixels)
- [ ] D1. `--screenshot` with the software rasterizer (`--disable-gpu`); Skia paints into
      a bitmap; write the PNG to the VFS and extract it. **Milestone: a rendered PNG.**
- [ ] D2. Fonts via our served fontconfig cache; verify text renders.

## Phase E — a browser window on the EuroOS desktop (from the old plan)
- [ ] E1. Full chrome (not headless) window through the in-kernel X server (reuse the GTK
      live-window path); route desktop kbd/mouse; present its framebuffer.
- [ ] E2. Load a local page, then euro-os.eu over the netstack.

## Method
- One boot per verified step; the NMI dump is the key new tool. No Claude trailers, no push.
- Under-claim: "engine works" != "app works". Keep the working system green (20/20).
