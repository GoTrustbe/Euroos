# Sprint 2: from submitted frames to a working Chrome UI on EuroOS

## Standing
The renderer draws and SUBMITS compositor frames (host-level counts). The capture
never answers because no CopyOutputRequest is ever issued, and the surface
numbers say why: GarbageCollectSurfaces 5 (host 1), EmbedSurface 3 (host 1) — the
browser embeds one surface while the renderer submits into another, and the one
being watched is thrown away.

## Rule for this sprint
Do not stop between steps. Every boot runs in the background; while it runs,
prepare the next probe or fix. Report only when the milestone flips or a decision
is needed. The sprint ends when a Chromium-rendered page is visible in a window
on the EuroOS desktop and reacts to input.

## Phase S — surface agreement (the one open blocker)
- S1. Extract the ACTUAL LocalSurfaceId values from the trace on both sides
  (embed vs submit): the trace carries them as "LocalSurfaceId(p, c, tok…)".
  Compare with the host run. -> tells us WHO is behind: browser parent seq or
  renderer child seq.
- S2. Fix the kernel gap that loses/delays the id agreement (candidates: message
  ordering on our unix sockets, a dropped resize ack, an event never delivered).
- S3. PNG round-trips. Milestone: a real Chromium-rendered PNG from a EuroOS run.

## Phase B — the frame on the desktop
- B1. PNG out of the log (hex dump exists) -> verified image on the host.
- B2. Serve the PNG bytes to the DESKTOP: decode in-kernel (or request raw
  bitmap format over CDP), PutImage into an X window on the live desktop.
  Milestone: the page visible in a window on EuroOS.

## Phase C — input
- C1. Window kbd/mouse -> Input.dispatchKeyEvent / dispatchMouseEvent over the pipe.
- C2. A click visibly changes the page. Milestone: interactive.

## Phase D — live loop
- D1. Page.startScreencast for continuous frames (with acks) instead of polling.
- D2. Address bar -> Page.navigate. Milestone: a usable browser window.

## Phase B breakthrough (2026-08-14): chrome's OWN browser window, mapped, alive
The x11 pivot works. In order, each named by the binary itself:
- fontconfig wall: chrome bundles a NEWER fontconfig (cache version 11); its
  cache is now written by chrome's own binary on the host, the stat family
  serves the exact dir/file mtimes it records (nanoseconds included), and the
  cache file stats newer than the dir. Cache VALIDATES: zero font opens, the
  FcCharSetFreeze crash is structurally unreachable.
- missing libX11-xcb.so.1: chrome's FATAL named it; the pack is rebuilt from the
  full ldd closure plus X shims (chrome-pack2.img, 95 files).
- RESULT: CreateWindow 800x600 at (40,40), **MapWindow**, a GC created — and the
  browser RUNS: profile init, segmentation platform, live safebrowsing URL
  requests. The EuroOS desktop has a real chrome window on its X server.

## The one blocker left before pixels-on-desktop
Chrome's own words: "Terminating current process after 15 seconds with no
connection" (child_thread_impl.cc:942) — the IN-PROCESS renderer's channel to
the browser never establishes; its watchdog then exit_group()s the whole
process (task 50, a worker, exits 0 — which looked like a clean quit until the
exitgrp dump named the thread). The channel bootstrap uses the SCM_RIGHTS
descriptor handoff, and the gscm test STILL fails in the suite while the
kernel-side readback of the written control message looks correct. gscm now
prints the receiver's raw view (controllen, flags, first 20 bytes) — the next
suite run gives the byte-for-byte diff between what the kernel wrote and what
glibc sees.

Iteration hygiene learned the hard way: the precheck experiment (running gscm
inside the fast chrome boot) hangs that boot in a way the suite never does —
reverted; debug where it reproduces deterministically.
