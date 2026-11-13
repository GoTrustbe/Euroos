# Chromium on the EuroOS desktop, picked up again (2026-11-12)

Branch `feature/chrome-workplace-2`, on `feature/integ-2026-11b` `6fcf24c`, with
`feature/app-control` `c6f033f` (integ-2026-11b plus `feature/linux-rand`) merged in.
Not merged, not pushed. Bugs: `../../LOADTEST-TRACKER.md` (BUG-090 to BUG-099).

Everything below was measured on the Dell R630 (KVM, `-cpu host`, 2 vCPU, 6144M,
QEMU 10.2.1, usb-kbd and usb-tablet over QMP, virtio-net on SLIRP without
`restrict=on`, image built with `ui.tour=off;update.check=off`), with
`chrome-pack2.img` and the NSS pack `nss-pack-vid.img`. Input is real USB-HID. Nothing
is sent to the page over DevTools; the kernel's DevTools pipe is used to ASK the browser
which pages it has open (`ctabs`).

**The NSS pack matters.** `/opt/euroos/packs/nss-pack.img` carries NSS 3.120 libraries
and fails against the chrome pack exactly as `scripts/mk-nss-pack.sh` warns
(`NSSUTIL_3.108 not found`, `FATAL:crypto/nss_util.cc:146 nss_error=-5925`, the browser
aborts). `nss-pack-vid.img` has the matching generation (plus the video test files).

## What was broken, and is fixed

| Bug | What the browser or the test said | Cause |
|---|---|---|
| BUG-090 | window empty for good; main thread in `poll()` on its first X connection | the X server never answered GetPointerMapping; since `poll(-1)` returns `-EINTR` xcb waits for it. This is also the old "X input start-up race": until October xcb gave that connection up for dead |
| BUG-091 | "Aw, Snap!"; `persistent_sample_map.cc:148 corrupt=1`, `disk_data_allocator.cc:213 Likely file corruption` | `rename` over an existing file removed its entry from the file table and moved every later file one slot down under the open descriptors |
| BUG-092 | Ctrl+L and typing did nothing; a click navigated somewhere else | keys eaten by the desktop's character loop; clicks sent twice (X and DevTools); no release edge, no wheel, `E0` keys mis-decoded, US keymap whatever the layout |
| BUG-093 | no copy and paste, no layout switch, no maximise; the page changed by itself after 2 and 5 minutes | no atoms, no selection owners, no MappingNotify, 1280x800 X screen; the DevTools visit list ran for every `chrome` |
| BUG-094 | (found by reading) | an event was dropped when its connection was being processed on another core |
| BUG-095 | 3 or 4 characters of a typed line missing | four unread X events triggered a 400-line diagnostic dump with interrupts off |
| BUG-096 | the second of two quick copy-and-pastes pasted the first text | no XFIXES selection events, so chrome's clipboard sequence number never moved |
| BUG-097 | a tooltip replaced the browser in its frame; maximise resized the tooltip | the frame showed "the window that painted last" |
| BUG-099 | a typed character showed 1.7 s later | the hosted window was recomposited on the 2-second status tick |
| (flags) | browser main thread died on address 0 at the tabs step | `--disable-features=OptimizationHints` leaves a null model provider that `ContextualTasksContextModelHandler` dereferences; the feature is no longer disabled |

## What works (with the run that shows it)

- **Start from the desktop and the reference site.** `chrome https://euro-os.eu/` typed in
  the Terminal: `baseline-euro-os-b7.png`. Time line of a start (run s9): Enter 0 s,
  window on screen 3.5 s, X connection 10.3 s, first child process 11.7 s, renderer
  17.9 s, first page painted and still about 35 s.
- **Interaction matrix, 19 of 19 steps** on the merged tree (`interaction-s10/`,
  `report.txt` there): omnibox (Ctrl+L, URL, Enter), typing in a form, Ctrl+A/C/V between
  fields, a word selected by double click and a stretch selected by drag (copy, paste),
  wheel / PageDown / End / Home, F5, be-azerty with AltGr after a layout switch while the
  browser runs (`azerty AZERTY 0123456789 m,;:=?./+ @#{}[]|€` arrives character for
  character), a second tab (open, load, switch, close), the browser's menu (opens at its
  button, Esc closes it), frame drag, maximise (X window 1518x1008) and restore, a link
  in the page, Alt+Left and the toolbar's forward button.
  Repeated on the last kernel of this branch: 19 of 20 steps in `perf2` (the twentieth is
  the key-latency step, below).
- **First full Chromium start on the kernel CSPRNG (BUG-080)**: that same run s10.
  NSS initialises, `https://euro-os.eu/` loads over TLS;
  `[entropy] CSPRNG seeded: 548 of 256 bits`.
- **Sites, each in a fresh browser session, three rounds** (`sites/`, numbers are the
  distinct colours in the content area 75 s after the navigation):

  | Site | Round 1 / 2 / 3 | Verdict |
  |---|---|---|
  | https://euro-os.eu/ | 1389 / 1389 / 1389 | renders, 3 of 3 |
  | https://nl.wikipedia.org/ | 798 / 813 / 798 | renders, 3 of 3 |
  | https://www.openstreetmap.org/ | 2284 / 2286 / 2284 | renders with map tiles, 3 of 3 |
  | a PDF (w3.org `dummy.pdf`, in the built-in viewer) | 140 / 140 / 140 | renders, 3 of 3 |
  | https://github.com/GoTrustbe/Euroos | 634 / 635 / 635 | renders, but the desktop loop stood still for 12 to 17 s during the load in every run (`WATCHDOG TRIPPED`, BUG-085's deadman): 0 of 3 |
  | https://www.youtube.com/ | 59 / 45 / 45 | loading skeleton only: 0 of 3 |
  | Nextcloud | not run | no public instance without a login was at hand |

  The PDF is not on euro-os.eu (the site has none); a public test file was used.
- **Twenty fresh boots** (`starts20.txt`): chrome started 20 of 20; text typed into the
  page right after the start arrived 19 of 20 at the first read (the twentieth had 8 of
  9 characters when the harness read once, without waiting); a URL typed in the omnibox
  committed 20 of 20 (3.7 to 5.3 s); the menu opened on a click 20 of 20. The harness
  marked all twenty FAIL on its colour threshold for the omnibox step, which it applied
  before the page had painted: a harness fault, corrected afterwards
  (`b6aea6e`), not rerun.
- **Page load of the reference site**, Enter in the omnibox to the final picture:
  median 1.35 s and 1.31 s (two runs of five, maximum 1.50 s) after BUG-099; 2.67 s
  (maximum 5.87 s) before.

## What does not work, and the wall

- **All sites in one session: BUG-098.** One fd space is shared by the browser and all
  its children, with 500 numbers for files: 245 in use after euro-os.eu, 380 after
  Wikipedia, 483 in GitHub, then `Creating shared memory ... failed: Too many open files`
  and `FATAL:shared_memory_switch.cc:111 Check failed: shmem_token`. Ruled out: slots
  pinned by deferred closes of inherited descriptors (patched, measured, no change).
- **GitHub: the desktop loop stalls 12 to 17 s** while the page loads (three of three
  runs). The page renders. Not located: the NMI taken afterwards shows a
  `Chrome_ChildIOThread` in the kernel after `ftruncate`; the link map of that image was
  overwritten by a later build, so the address is not named.
- **YouTube: W19 is not reached.** The front page stays in its loading skeleton and a
  typed watch URL never commits (90 s). The renderer says why:
  `ERROR:v8_initializer.cc:971 V8 process OOM (Zone)`. 16 of 18 requests finished, no
  request failed, no kernel fault. The real-click gesture could therefore not be tried;
  no picture, no sound measured.
- **Key to screen under 100 ms: not shown.** Before BUG-099: median 1742 ms (27 keys).
  After it the latency step of the harness registered no change in two runs and its
  keys did not arrive, although held keys and typed text do arrive on the same image
  in a probe. No number.
- **Two-hour run: not met.** The browser died in the first round, four minutes in, of
  BUG-098 (`soak.txt`). The guest stayed up for the 43 minutes the run was left
  running: no kernel fault line, no deadman line, and the kernel's memory account
  flat from the tenth heartbeat on (heap 120 MiB, big-block pool 477 of 511 MiB, RAM
  files 494 MiB). That last figure is its own warning: two sites put 494 MiB of
  profile, cache and shared-memory files into kernel memory.
- A held key does not repeat (the X server sends one press); a popup is clipped to
  the browser window; a paste from another X client is empty.

## Regression checks

- Host tests: 187 suites, 1331 passed, 0 failed, 1 ignored (`hosttests.txt`).
- Self-test image under TCG, `user-smp4-iommu` and `user-smp1-noiommu`:
  `markers-compare.py` against `../integ-2026-11/`: 0 worse, 7 tags new (those of
  integ-2026-11b and linux-rand); against `../linux-rand-2026-11/` (smp4): 0 worse, 0
  new. `[lk] boot-total order inversions=0 ... task-lock-with-IF0=0` in both
  (`markers-final.txt`).
- `gui-smoke.py` on the public image under KVM: PASS, 66 of 66 (`gui-smoke.txt`).

## Files

`baseline-euro-os-b7.png` first render after BUG-090 · `omnibox-nl-b11.png` the page
reached by typing in the omnibox · `maximised-azerty-b13.png` maximised, AZERTY text in
the field · `menu-b21.png` the menu over the page · `interaction-s10/` the 19-step run
· `sites/` one screendump per site (round 3) and the eighteen run logs ·
`starts20.txt` · `perf-*.txt` · `soak.txt` · `hosttests.txt`, `markers-final.txt`, `fd-usage-b22.txt`, `youtube-yt1.txt`,
`gui-smoke.txt`.
