# Chromium on EuroOS, W23: renderers that start, a desktop loop that is not taken for idle (2026-11-13)

Branch `feature/chrome-w23`, from `feature/chrome-fdspace` `40f853a`. Not merged, not pushed.
Bugs: `../../LOADTEST-TRACKER.md` (BUG-134, BUG-135, BUG-136 closed; BUG-140 to BUG-147 new).

| Commit | What |
|---|---|
| `bb2e7d8` | the tests first: `[churn-forkthreads]` (`gslots`, glibc with fork) and the host test of the arena layout. Both FAIL on the tree before |
| `7b0629c` | the kernel: BUG-140 (task slots of ended fork children; the cause of BUG-134/135), BUG-141, BUG-142 (64 MiB arena layout), BUG-143 (the desktop loop taken for idle: the TCG watchdog trips), BUG-144 (lock-free NMI probe), the stopwatch steps and tick samples, `[keylat]` |
| (last) | this proof, the tracker, `CHROME-ON-EUROOS.md` |

Images: `fix0b` = `bb2e7d8` (the tree before, with the tests), self-test profile; `base-pub` = `40f853a`, public
(`ui.tour=off;update.check=off`); `w23g-*` = the code of `7b0629c` (self-test, public with those two options, plain
public). Intermediate images (`w23a` to `w23f`) carried the same fixes plus diagnostics in progress; where a number
comes from one of them it says so.

Machines: R630 (CPUs 0-13 of this team): KVM `-cpu host` for the browser and the self-test boots, TCG
(`qemu64,+smep,+smap`, each QEMU pinned) for the TCG boots; build server .16: TCG (nice 10). Browser runs: 2 vCPU,
6 GiB, usb-kbd and usb-tablet over QMP, virtio-net on SLIRP without `restrict=on` (matrix sites only, update check
off), `chrome-pack2.img`, `nss-pack-vid.img`. Every other QEMU: `restrict=on` or no NIC.

## 1. W23 = BUG-134/135: every new renderer crashed after the PDF viewer, then typed URLs stopped navigating

**Reproduced with three sites.** `chrome-smoke.py --only sites --sites pdf,euro-os,wikipedia --rounds 5` on
`base-pub` (run b1): round 3, after the pdf: four `[abort] tN "CrRendererMain"`, then every typed URL stayed on its
page (7 steps FAIL, `chrome-runs.txt`). Every failing run of this sprint and the last one reached task numbers t250
to t255.

**Cause (BUG-140).** The scheduler has 256 task slots. Only a worker thread that ended itself with exit(2) gave its
slot back. The main task of a fork child, and every thread that was still waiting when the child ended (exit_group
of its main or of a worker, kill, abort), kept its slot until the browser ended. A renderer has 15-25 tasks; after a
few sites (sooner with the PDF viewer, which starts more processes) the table was full, `pthread_create` answered
EAGAIN and a new renderer died in its first CHECK on a thread start (the message is in the kernel log only; in an
older run: `pthread_create: Resource temporarily unavailable (11)`, `Check failed: io_thread_->StartWithOptions`).
The browser could not start what a navigation needs either: BUG-135. Address reuse was not involved (ruled out
before).

**Fix.** `retire_task()`: kernel stack, futex entries, the registries that name a task by number, then the slot is
offered again, in both ways a fork child ends. A reused slot starts without the capabilities, syscall profile and
mark of its previous occupant. BUG-141: a fork refused late now gives the child's arena back. `fds` prints the
table: `scheduler: N of 256 task slots handed out, L live, R free for reuse, H held by ended tasks`.

**Test first** (`bug140-churn.txt`, KVM 4 GiB):

| | `[churn-forkthreads]` |
|---|---|
| before (`fix0b`) | `32 of 40 rounds ... round 32 (mode 2): the child could not create its threads (pthread_create 11) -> FAIL (exit 1) · task table 39→256 → FAILED` |
| after (`fix1b`) | `40 of 40 rounds (280 tasks of ended children), bad=0 -> PASS (exit 88) · task table 34→34 → OK ✓` |

**In the browser** (`chrome-runs.txt`):

| Run | Image | Sequence | Navigations | Aborts | Scheduler table at the end |
|---|---|---|---|---|---|
| b1 | before (`base-pub`) | pdf, euro-os, Wikipedia × 5 | 7, then none (7 FAIL; stopped after 14 of 15 steps) | 4 | t255 reached |
| a1 | `w23b-pub` | same | 15 of 15 | 0 | 223 handed out, 214 live, 0 held by ended tasks |
| a2 | `w23g-pub` | same | 15 of 15, **PASS 18/18** | 0 | see file |
| m1 | `w23b-pub` | full matrix × 3 + perf | 18 of 18 | 0 | 199 handed out, 188 live, 0 held |
| m3 | `w23g-pub` | full matrix × 3 + perf | 18 of 18 | 0 | 219 handed out, 208 live, 0 held |

**Criterion "the site matrix three rounds in one session without a crash, typed URLs keep working": met** (m3, m1,
a2): no abort, every typed URL navigated, the scheduler census after every site "0 held by ended tasks". The steps
that failed in a1 and m1 were one Wikipedia step each on the console echo of the harness (1087 and 1047 ms, the
kernel stopwatch said at most 244 ms in that step; the R630 ran ten TCG guests of this team beside them); m3 and a2
had none.

Two things the matrix counts as "ok" that are not, both harness (`feature/chrome-harness`):
- **BUG-145:** after YouTube, `https://www.openstreetmap.org/` typed right after Ctrl+L arrives as
  `s://www.openstreetmap.org/` (a Google search): the browser gives Ctrl+L to the page first and focuses the omnibox
  when the busy renderer answers; what is typed before that goes to the page. The step passes because the host is in
  the search URL. Harness: wait for the omnibox selection after Ctrl+L; judge the URL the tab shows.
- **YouTube consent:** in m3 the consent dialog was still open in the screendump of all three YouTube steps. The
  harness clicked at (411, 713) and (379, 695) before the dialog had appeared (its button finder matched the page's
  grey skeleton). Those three YouTube steps do not count. A separate session that answers the dialog with a real
  click once it is there: section 5.

## 2. BUG-136: the TCG wedge on the scheduler lock (smp 4, 512 MiB)

**Not reproduced:** 0 wedges in 29 boots of `fix0b` (user-smp4-iommu, user-smp4-noiommu and kernel-smp4 at 512 MiB: 21 on the
R630, 8 on the build server), against 1 in 10 in chrome-2026-11b. What its trace showed, and what changed:

- It wedged right after `gsdl` started in a **64 MiB fallback arena** whose mmap window began past the end of the arena
  and whose brk region ran over its stack (BUG-142). In every 512 MiB boot, before: `mmap anon 8 KiB -> ENOMEM: arena
  mmap window full (break 0xa250000, end 0x9f0c000)`, `gsdl (SDL2): exit=139`. After (`euromm::arena::glibc_layout`,
  host-tested, `bug142-layout-test.txt`): `GSDL: SDL_Init ok`, `GSDL: window created`, `gsdl (SDL2): exit=0`.
- The fault path waits for the scheduler lock (`sched::current()` in the repeated-fault line), CR2 was 0x1000: a
  fault taken with that lock held cannot end. The one read the scheduler made under its lock through an address
  from the interrupted frame (the byte before rip, BUG-143) is gone.
- The NMI probe stopped in its own `sched::current()`; it now prints its stack scan and census (BUG-144).

After: 16 boots of `w23g` at 512 MiB with smp 4 (8 on the R630, 8 on the build server; `bug136-boots.txt`): every one to the desktop, `gsdl (SDL2): exit=0` in each, no probe needed, no watchdog trip. Before (`fix0b`, 29 boots: 21 R630, 8 build server): `exit=139` in each, and no wedge either.

## 3. WATCHDOG TRIPPED in kernel-smp1 under TCG (BUG-143)

**Found with the stopwatch.** New: every `[desk]` line names the longest step of the pass (the loop's work split into
named steps), and for a pass over a second where its timer ticks went. On the R630 a third of the TCG boots of the
self-test image tripped (`watchdog-tcg.txt`), for example:

```
[desk] #2 the desktop loop was away 34 ... longest step: repaint: windows 26623 ms; ticks of the loop at rip
  0x1045825a x1, ... 0x10456f76 x8 ...; ticks of other tasks: t10 x517, t11 x517, t7 x517, t8 x517, t9 x517,
  t4 x54; the loop meanwhile: Ready x2649, Sleeping x0, Blocked x0
```

The loop was ready all the time and got 13 ticks of 2 600; its rips are soft-float (`__addsf3`, `__mulsf3`) under
`drop_shadow` and the text renderer, five CPU-bound self-test processes had the rest.

**Cause.** The scheduler called a tick of the desktop loop idle when the byte before the interrupted rip was 0xf4
(hlt). 0xf4 is also -12 as a displacement or immediate (`lea -0xc(%rbp)`), common in that soft-float code: the busy
loop was taken for idle, lost its turn at every tick (BUG-127) and waited for its vruntime against tasks of nice -10.
Under KVM a pass is short and rarely hit; under TCG the drawing takes seconds of soft-float.

**Fix.** The loop says when it is idle: `desktop_idle_hlt()` sets a flag around its hlt.

| TCG boots of the self-test image (R630) | boots | watchdog trips | longest gap between two passes |
|---|---|---|---|
| before (hlt-byte test: `fix0b`, `w23a`-`w23f`) | 81 (kernel-smp1: 48) | 22 (kernel-smp1: 14) | up to 43.6 s |
| after (`w23g`), kernel-smp1 | 12 | **0** | 2.0-4.0 s |
| after (`w23g`), TCG matrix of 10 | 10 | **0** | 2.1-2.5 s |
| same matrix, before (`fix0b`), same conditions | 10 | 2 (user-smp1-iommu, user-smp1-noiommu) | 18.4 s, 33.5 s |
| after (`w23g`), every TCG boot of this round (R630 and build server) | 35 | **0** | 4.0 s |

On the build server the same images of the tree before tripped 3 times in 8 boots.

## 4. Key to screen (BUG-146, not met)

`[keylat]` (new): per key the time from its make code in the scancode ring to the X client, the X queue to the
client's read, the read to the client's first draw, that draw to the screen; `keylat` in the shell gives the
medians (`keylat.txt`):

| | harness median | kernel: ring → X client | X queue → read | read → first draw | draw → screen |
|---|---|---|---|---|---|
| chrome-2026-11b | 292 / 335 ms | — | — | — | — |
| m1 (`w23b`) | 369 ms | 0.1 ms | 4.0 ms | 156 ms | 25 ms |
| m3 (`w23g`) | 476 ms | 10.5 ms | 3.9 ms | 206 ms | 17 ms |

The X server and the desktop take 15-30 ms of it; the browser's own path from the X event to its first draw takes
75-210 ms, and the harness counts until the draw that shows the character (the first draw is often the caret), plus
the USB path and its own 23-26 ms screendump. All of the browser runs on one CPU (ring 3 only on the BSP), with 30
ready threads and 10 ms slices.

**Tried and rejected: a wake hand-off** (a system call that wakes a task yields at its end). In one session (ab1,
`w23e-pub`) with it off, then on: woken tasks waited < 0.1 ms in 17 809 of 19 670 wakes instead of 1-10 ms, but the
harness median went from 483 ms to 1 102 ms (22 of 30 keys within 2 s), and with it on from boot (m2) page loads
slowed and a site did not finish: the threads that wake others gave their turn away and waited a round of thirty
for the next. It stays in the kernel, off (`wakeyield on|off`). Kept: after the hosted window drew, the next yield of
any task goes to the desktop loop.

**Not met:** < 100 ms. Next: more than one CPU for ring 3, or a shorter slice for the threads that do the input path.

## 5. YouTube consent with a real click

In m3 the harness left the consent dialog open (section 1), so the dialog was answered in a separate session (`drivers/ytconsent.py`, `w23g-pub`): YouTube opened, the page settled, the dialog scrolled to its buttons, a QMP click on **Reject all**: the dialog closed and YouTube showed its home page with "Your YouTube history is off" (`screenshots/youtube-consent-dialog-before-click.png`, `screenshots/youtube-after-reject-all-click.png`). The button reacts to a real click: no input bug. On the second visit in that session no dialog came (the answer was kept). No login, no account.

## 6. Regression round

| Check | Result |
|---|---|
| `cargo test` on the host (`cargo-test.txt`) | 187 test binaries, **1 350 passed**, 0 failed (`euromm::arena` new: 3) |
| TCG matrix of 10, `w23g-selftest`, R630, `restrict=on` (`tcg-*.markers`, `comparison-tcg.txt`) | **10 of 10** to the desktop; 0 watchdog trips, 0 probes, 0 crash lines; `[churn-forkthreads]` OK in every boot ("fork refused for memory" at 512 MiB and 1 GiB); gsdl `exit=0` at 512 MiB |
| `markers-compare.py` against `../chrome-2026-11b/` | same conditions (`fix0b` before against `w23g`, R630, `restrict=on`, `comparison-tcg-same-conditions.txt`): **0 worse in 10 of 10**, `3g2-wire` FAILED → OK in two. Against `../chrome-2026-11b/` (`comparison-tcg.txt`): 0 worse in kernel-smp1 and kernel-smp4; in the eight others `udrv-net`, `udrv-crash` (and at 1 GiB `udrv-linux`) OK → FAILED ("no DNS reply", "connect failed": `restrict=on`, the base ran without it; identical in the before matrix here), and at 1 GiB `fork FAILx8 → FAILx9`: the fork of the new `[churn-forkthreads]` that 1 GiB refuses (its verdict "fork refused for memory: one process only", OK) |
| `gui-smoke.py` on the plain public image (no boot options, 2 vCPU, 4 GiB, `restrict=on`, `gui-smoke-public.txt`) | **PASS 67/67** |

## 7. Not done

- Key to screen < 100 ms (BUG-146).
- BUG-147: after BUG-140 no slot is held by an ended task, but the browser keeps up to 16 processes: 214 of 256 live
  tasks in a1; the thread kernel stacks (224) are as close. More slots need memory that small machines do not have
  as static arrays.
- BUG-145 and the YouTube consent finder are harness work (described above).
- BUG-137 (demand pool) and the harness: other teams.

## 8. Deviations from the rules

- **TCG boots on the R630** (the matrix of chrome-2026-11b ran on the build server, without `restrict=on`): here every
  TCG NIC has `restrict=on`, so network self-tests that need the internet differ from the base for that reason only
  (`udrv-net`, `udrv-crash`, `udrv-linux`: identical in the before matrix run here).
- Test drivers outside the tree (`/srv/euroos/w23-runs/drivers/`: the A/B of the wake hand-off, the YouTube consent
  check, a run summary) use `chrome-smoke.py` as a module; the harness itself is unchanged.
- The musl test binaries the kernel embeds are not built on .16 (no musl-gcc): copied from the build of `40f853a` in
  `wt-chrome2` into this worktree (untracked, as there).
- Run b1 (the tree before) was stopped by its QEMU PID after 14 of its 15 steps, once the failure was clear; for
  about a minute it shared CPUs 0-1 with the start of m1.
- The browser runs a1, m1 and m3 ran while up to ten TCG guests of this team used CPUs 4-13 of the R630.
- Screendumps, the driver scripts and this README passed through the laptop (scp, removed afterwards); no source code
  was stored there.

## Files

`bug140-churn.txt` · `bug142-layout-test.txt` · `chrome-runs.txt` (every browser run: image, aborts, task census, one
line per step) · `keylat.txt` · `watchdog-tcg.txt` (every TCG boot of this sprint on the R630, and the samples of the
long passes) · `bug136-boots.txt` · `tcg-*.markers`, `comparison-tcg.txt` · `gui-smoke-public.txt` ·
`cargo-test.txt`. Serial logs stay on the R630 (`/srv/euroos/w23-runs/`).
