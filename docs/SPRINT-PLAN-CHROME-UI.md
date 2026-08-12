# Sprint: a working Chrome UI on EuroOS

## Goal
A real Chromium page, visible in a window on the EuroOS desktop, that responds to
the mouse and keyboard. Not a mockup, not a still: a browser you can use.

## Where we start (2026-08-12)
- Chromium (`chrome-headless-shell`, 180 MB, demand-paged from disk) RUNS: it
  navigates, loads resources, executes JS, and emits the DOM. `--dump-dom` works
  unmodified, and the kernel can also drive DevTools itself over a pipe.
- NOTHING is ever painted: no compositor frame is produced, so every screenshot
  request returns nothing. With a capture pending, chrome's threads sit IDLE on
  53-74 s housekeeping timers, so the request never becomes scheduled work.
- The EuroOS side already has what a UI needs: an in-kernel X server with windows,
  PutImage and real keyboard/mouse input, plus a compositor that blits to the
  framebuffer.

## The shape of the answer
Chrome does not have to draw its own window for the UI to be real. The reachable
architecture is the one remote-browser products use:

    chrome (headless, renders offscreen)
        --- frame as PNG/bitmap over DevTools --->  EuroOS window (blit)
        <--- mouse/key events over DevTools    ---  EuroOS input

Everything in that loop exists here except the frame. So frames come first, and
they are the only genuine unknown; the rest is work we know how to do.

## Phase A — FRAMES (the one real unknown)
- **A1.** strace the HOST across a capture and diff it against ours: which
  syscalls does chrome make between "captureScreenshot sent" and "PNG returned"?
  Anything we answer differently (or not at all) is the gap. *(timerfd already
  ruled out: chrome never calls it here, so delayed tasks ride on epoll timeouts,
  which we now honor.)*
- **A2.** Fix whatever A1 names; repeat until a PNG comes back.
- **A3.** If A1 names nothing, force the issue from the other side: drive frames
  explicitly (`--enable-begin-frame-control` + `HeadlessExperimental.beginFrame`)
  and make THAT path work — on the host it changes the rendering contract, so it
  needs its own driver loop rather than a flag.
- **Milestone: a PNG of a real page, decoded on the host, from a EuroOS run.**

## Phase B — the frame on the desktop
- **B1.** Decode the PNG in the kernel (or ask chrome for a raw bitmap over CDP,
  which avoids a decoder entirely — `Page.captureScreenshot` returns PNG, but
  `HeadlessExperimental.beginFrame` and screencast frames can be simpler).
- **B2.** Create a real window through the in-kernel X server and PutImage the
  frame into it, so it lands on the live desktop next to the other apps.
- **Milestone: a browser-rendered page visible in a window on EuroOS.**

## Phase C — input
- **C1.** Route the window's keyboard and mouse events (the pump that already
  feeds X clients) into CDP `Input.dispatchMouseEvent` / `dispatchKeyEvent`.
- **C2.** Verify a click changes the page (a link, a button, a text field).
- **Milestone: the page reacts to the user.**

## Phase D — a live browser
- **D1.** Continuous frames: `Page.startScreencast` delivers frames as they change
  (with acks), which is exactly the loop we want and avoids polling captures.
- **D2.** An address bar in the window: type a URL, `Page.navigate`, new frames.
- **D3.** Keep it honest about what it is: chrome renders offscreen, EuroOS shows
  and drives it. Say so in the UI and the docs.
- **Milestone: a usable browser window on EuroOS.**

## Rules
- One boot per verified step; let the binary name each blocker; fix, then commit.
- Validate every protocol move on the host oracle FIRST, so the guest is never
  debugged against a guessed protocol.
- Every kernel fix gets a test in the boot suite that fails on the old kernel.
- No Claude trailers. Do not push. Under-claim: "renders offscreen and we show it"
  is the honest description, and it is worth plenty.
