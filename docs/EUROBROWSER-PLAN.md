# EuroOS — Sprint AB: EuroBrowser

*A browser for EuroOS. Two parallel tracks, honestly documented.*

**Status:** ⬜ planned (AB-B1 tokenizer started)
**Priority:** high — users expect a browser
**Kind:** `N 🏗️` — large new subsystem, two phases
**Source:** user-supplied `SPRINTABEUROBROWSER1.md` (2026-06-06), re-grounded against the actual EuroOS crate layout.

---

## The decision

**No debate: EuroOS gets a browser.** Two tracks at once, each honest about what it is:

- **Track A — Firefox bridge.** Get Firefox running through the existing musl
  compatibility layer by stubbing the libraries it `dlopen`s onto EuroOS primitives.
  Pragmatic, short-term, accrues stub debt on purpose.
- **Track B — EuroWeb, own engine.** A browser engine from scratch in Rust, fully under
  EuroOS control. This is the real endpoint and the one that matches the project's
  identity (sovereign, memory-safe, `#![forbid(unsafe_code)]`, EuroTLS/EuroLocale-native).

Track A is what users need *today*. Track B is what EuroOS architecturally *deserves*.
We invest most engineering in B and treat A as a removable bridge.

---

## Grounding: what EuroOS actually exposes (not X11/EGL/D-Bus)

The uploaded plan was written against a generic Linux. The real targets in this repo:

| Generic dep in the doc | EuroOS reality it must map to |
|---|---|
| `libX11` / Wayland | `eurowl` (Wayland-ish protocol) + `eurodisplay` server (`/run/eurodisplay.sock`, AF_UNIX, H2 live display server already built) |
| `libEGL`/`libGL` | software raster → `eurodisplay` framebuffer (no GPU path yet; K4 later) |
| `libdbus` | `euroipc` — stub D-Bus to NULL so apps fall back gracefully |
| `libpulse` | `euroaudio` (Intel HD-Audio driver present) |
| networking | `euronet` (own TCP/IP) + `eurotls` (TLS 1.3, EU trust store) |
| sandbox | `eurosandbox` / EuroGuard capabilities (`Container::effective_caps`, `allow_connect`) |
| locale | `eurolocale` (24 EU languages) |

So Track A is a **compat-stub crate**, and Track B is a **new `euroweb` crate** that
consumes the sovereign stack directly — no stubs, no foreign TLS/ICU.

---

## Track A — Firefox on EuroOS (the bridge)

### Approach: stub-library layer
Don't rebuild Firefox; implement the missing `.so`s as thin shims that forward to EuroOS
primitives. Only implement the symbols Firefox actually calls (found via an
`strace`-equivalent on the musl bridge), not the full 1200-function Xlib.

```
libs/compat/
├── libX11/      XOpenDisplay/XCreateWindow/XNextEvent/XPutImage → eurodisplay window+events+blit
├── libEGL/      eglGetDisplay/eglCreateWindowSurface/eglSwapBuffers → software framebuffer → eurodisplay
├── libGL/       OpenGL → software-raster fallback
├── libdbus/     dbus_bus_get → NULL (EuroOS uses euroipc; Firefox degrades gracefully)
└── libpulse/    pa_simple_new/write → euroaudio stream
```

The critical bridge is **XPutImage / eglSwapBuffers → `eurodisplay` framebuffer present**:
Firefox software-renders into a buffer, we blit that buffer to its EuroOS window.

A `user.js` profile forces software mode and kills telemetry:
`gfx.webrender.enabled=false`, `layers.acceleration.disabled=true`,
`dom.ipc.processCount=1`, all `toolkit.telemetry.*=false`,
`datareporting.*=false`.

### Steps (Track A)
- **A1 — dependency audit:** run the Firefox binary, log every failing `dlopen`/missing symbol → minimal stub list (~40–60 X11 fns typically).
- **A2 — libX11 minimum stubs** → `eurodisplay` windows + event queue.
- **A3 — EGL software surface** → pixels to `eurodisplay` framebuffer.
- **A4 — libdbus + libpulse stubs** so it doesn't crash on missing services.
- **A5 — iterate to a window:** start, crash, stub the missing symbol, repeat.
- **A6 — EuroGuard sandbox:** Firefox gets `CAP_NET`, `CAP_DISPLAY`, `CAP_FILE_READ` (downloads only) — **no** `CAP_EXEC`, **no** `CAP_VAULT`. Enforced via `eurosandbox::Container`.

**Verify A:** Firefox opens a window in `eurodisplay`; `https://euro-os.eu` loads via
`euronet`; mouse/keyboard/scroll work; audio via `euroaudio`; EuroGuard blocks
out-of-capability actions.

**Honest cost:** ~5–7 attended sessions. Result: Firefox runs, with stub debt.

---

## Track B — EuroWeb (the sovereign engine)

### Why own it
Full codebase control · zero foreign deps (X11/D-Bus/ICU/NSS stubs are debt) ·
native EuroGuard per-tab sandbox · **EuroTLS as the only TLS** · **EuroLocale native** ·
EUPL-1.2 throughout · lighter and faster on EuroOS hardware long-term.

### Crate: `crates/euroweb` (new, no_std + alloc, host-testable)
The engine is pure logic → host-tested like every other EuroOS core. The browser shell
(`userspace/euroweb` or a kernel `webview` module) wires it to `eurodisplay`/`euronet`.

```
crates/euroweb/
├── html/    tokenizer (HTML5 state machine) · parser (tree construction) · dom · quirks
├── css/     tokenizer · parser · selector (specificity/cascade) · computed · properties
├── layout/  block · inline (line-breaking) · flex · table · positioned
├── paint/   display_list · raster · text (reuse AA font rasterizer) · image · composite
├── js/      lexer · parser · interpreter (tree-walking; NO JIT — too much attack surface)
├── net/     ResourceLoader over euronet · http · cache (eurofs) · cookie · csp
└── security/ per-tab eurosandbox · same-origin · CSP · cert viewer (eurotls)
```

### Phased build

**Phase B1 — static HTML + CSS, no JS.** The first goal: render static pages correctly.
Good enough for euro-os.eu docs, intranet, parts of Wikipedia/gov sites.
- HTML5 tokenizer + tree construction (WHATWG Living Standard; ~3k lines, fully specced).
- CSS cascade: selector matching + specificity + author/user/UA order.
- Block + inline layout (covers ~80% of the web).
- Font rendering reuses the existing AA rasterizer.
- **Host tests:** HTML5lib tokenizer cases, css-selectors matching, box-model math.

**Phase B2 — flexbox + interactivity.** CSS Flexbox · `<form>` controls · link
navigation · HTTP redirects + history · cookies · `<img>` PNG/JPEG/WebP/SVG · HTML5
media API via `euroaudio`. After B2 most static sites work.

**Phase B3 — JavaScript.** Tree-walking interpreter first (never JIT). Each tab gets its
own `JsInterpreter` with a restricted `AgentCaps`: `fetch()` requires `CAP_NET`,
`localStorage` is per-origin-sandboxed in EuroFS, no kernel syscalls. Reuses the same
capability model as EuroAgent.

**Phase B4 — modern web.** CSS Grid · animations/transitions · WebSocket · IndexedDB→EuroFS ·
Service Workers (EuroAgent integration) · WebCrypto → reuse EuroTLS crypto.

### Browser UI & sovereign features
Tabbar · toolbar (← → ↻ ✕ home) · address bar · status bar with a **security badge**:
🔒 green = EuroTLS 1.3 + EuroCA/EU root · 🔒 yellow = valid, non-EU root · ⚠️ orange = HTTP ·
🚫 red = invalid/blocked. Privacy on by default (no extensions needed): block third-party
cookies, fingerprinting, tracking pixels, ads (easylist+easyprivacy bundled);
`save_passwords_to_vault=true` (EuroVault); **no telemetry, no cloud sync — ever.**

### Verify B
- B1: `euroweb https://euro-os.eu/docs/` renders correctly; tokenizer ≥90% HTML5lib;
  selectors ≥95% css-selectors suite; block layout box-model correct (all host-testable).
- B3: wikipedia.org navigable; `fetch()` without `CAP_NET` → `SecurityError` in console.

---

## Transition plan
EuroWeb B2 done → Firefox deprecated but available. EuroWeb B3 done → Firefox removed
from the default install. EuroWeb becomes the one, fully-sovereign browser of EuroOS.

## Comparison
| | Firefox bridge (A) | EuroWeb (B) |
|---|---|---|
| Available | weeks | months |
| Codebase control | none (Mozilla) | full |
| EuroGuard | via stubs | native kernel |
| EuroTLS / EuroLocale | no (NSS/ICU) | yes |
| Licence | MPL-2.0 | EUPL-1.2 |
| Tech debt | high | none |
| Web compat | excellent | grows with the engine |
| Sovereign | partial | full |

---

*First build target (this sprint): `crates/euroweb` HTML5 tokenizer + DOM, host-tested —
the foundation of Track B and the piece that proves the sovereign approach.*
