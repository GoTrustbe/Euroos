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
