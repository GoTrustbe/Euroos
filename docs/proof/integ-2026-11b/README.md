# Integration of the kernel debug round and the desktop pass, and BUG-065 (2026-11-12)

Branch `feature/integ-2026-11b` on `feature/app-control` `7858b72` (the head of
that branch when the work started and when it ended). Not merged into
`feature/app-control`, not pushed. Bugs: `../../LOADTEST-TRACKER.md` (BUG-065
fixed; BUG-080 to BUG-084 new).

| Commit | What |
|---|---|
| `1fcd49c` | merge of `feature/kdebug` (`931fe66`) |
| `1c8eb91` | merge of `feature/uidebug` (`03286a0`) |
| `b121e06` | the process self-tests of the kernel debug round are called from `main.rs`; the lock screen clears the window clip |
| `35a60c5` | **BUG-065**: the CSPRNG is seeded until it has its 256 bits, from mixed sources, and refuses otherwise |
| `c0db228` | `gui-smoke.py`: the BUG-063 step from the desktop Terminal, `--net e1000e`, the drag step waits for the guest |
| `6fcf24c` | **BUG-083**, **BUG-084**: two self-tests left the keyboard and the mouse of the self-test image in a wrong state (found by the CI q35 matrix of this round) |
| (last) | this proof, the tracker, `GUI-TESTING.md`, `BOOT-TIME.md`, `STATUS.md`, `SPRINT-PLAN-PHASE3.md` |

**Which image was tested.** Two rounds. The first ran on the images of
`c0db228` and ended when the q35 matrix showed BUG-083 and BUG-084. Everything
was then run again on the images of `6fcf24c`: self-test image sha256
`91fa63b98d38…`, public image `8f500a127a73…` (the commit after it changes
documents only). Every result below is of that second round unless it says
"first round". Serial logs stay on the build servers (`*.log` is not
committed); the `.markers` files hold the verdict lines of each log plus its
`[lk]`, `[fork]` and `[boot-time]` lines.

## 1. The merge

`git merge --no-ff feature/kdebug`, then `feature/uidebug`.

- **One textual conflict:** `docs/LOADTEST-TRACKER.md`. Both branches added bug
  entries after BUG-059; both are kept, BUG-060 to 065 before BUG-070 to 077.
  The run table and the status lines of BUG-052 to 058 merged by themselves.
- **No file with code was changed by both branches.** The expected meeting
  points were looked at one by one:
  - `xhci.rs` belongs to the desktop pass alone (wheel, right Alt); BUG-064 of
    the kernel debug round is in `audio.rs`.
  - `scripts/wizard-install-test.py` (desktop pass only), `scripts/build.sh`
    and `scripts/repro-build.sh` (kernel debug round only).
  - `lockscreen.rs` (kernel debug round) against `graphics.rs` and
    `compositor.rs` (desktop pass): see below.
- **The lock screen and the clip rectangle.** BUG-056 paints the lock screen's
  background with `fill_rect`, one call per run of a colour; since BUG-058
  `fill_rect` is cut to the framebuffer's clip rectangle. The clip is set and
  cleared inside `compositor::draw_window_body` and is not set while the lock
  screen paints, but `lockscreen::render` now clears it first, so that this
  does not depend on what the desktop did before (`b121e06`).
  - *Same picture:* `[ag-lock] … background 0 of 15360 sampled pixels off the
    per-pixel gradient` in every self-test boot of this proof. The frame of the
    self-test image under KVM, `-rtc base=2026-11-12T10:00:00`, against the two
    frames in `../kdebug-2026-11/screenshots/` (`lockscreen-pixel-compare.txt`):
    **0 differing pixels of 2 073 600** against `lock-kdebug-kvm.png`, and 0 against
    `lock-base-kvm.png` (`screenshots/lock-integ-kvm.png`). Under TCG,
    `-rtc base=2026-11-12T10:00:50`, against `lock-kdebug-tcg.png`: 166 pixels
    differ, all inside (1098,654)-(1123,663), the clock on the card (this
    frame was painted in another minute); 0 outside that box.
  - *As fast:* one frame takes 250 ms under KVM (kernel debug round: 180 to 310)
    and 2.3 to 2.9 s in the ten TCG matrix boots (2.6 s); the `lockscreen` phase
    there is 4.8 to 6.2 s (5.0 to 5.9 s) (`tcg-summary.txt`).
- **The self-tests of the kernel debug round** (`[bug063]`, `[fdspace]`,
  `[bug053]`) hung on `ring3::spawn_window_selftest`. `main.rs` calls them now,
  in the same place and order, under `SELFTEST` (`b121e06`). OK in all ten
  matrix boots; the release image prints none of them.
- **What the merge broke, and nobody had run:** the desktop pass did not run the
  CI q35 matrix. On the merged tree two of its legs failed, and they fail on
  `feature/uidebug` alone as well (`q35-legs-by-branch.txt`): BUG-083 and
  BUG-084, section 4.

## 2. BUG-065: the generator was not seeded in some KVM boots

### How often, before the fix

The merged tree before the fix (`1c8eb91`), R630, KVM, `-cpu host`, 204 short
boots (`bug065-before.txt`): `[3d8]` FAILED in **13** (6.4 %), the generator
unseeded for the whole boot in **11** (5.4 %), `[3d7]` FAILED in those 11.
(`[3d3]` comes 100 s later in the boot; these boots were stopped after
`[3d7]`.) The kernel debug round had 8 and 4 of 100.

### The cause

Three things together (`bug065-diagnosis.txt`,
`bug065-samples-failing-boots.txt`; the diagnostic code is `entdiag.rs.txt`,
never part of the kernel):

1. **The noise source measured a clock beat.** `gather_jitter` timed one
   multiplication between two RDTSCs, about 20 cycles. A batch of 512 samples
   held two to four different values. In most boots: `12, 12, 12, 12, 38, 12,
   12, 12, 14, 38, …`, a 38 about every fifth sample; in others `18, 18, 18,
   19, 18, 22`; and in the boots that failed `24, 24, 24, …` with a `28` every
   few dozen samples. That is the core's clock against the TSC's: which
   pattern a boot sees depends on the frequency the host has the core at in
   that millisecond, and it changes from batch to batch within one boot
   (credit per batch in one failing boot: `0, 2, 3, 62, 1, 1, 2, 2, 56, 1, …`).
   With all 40 hyperthreads of the host busy a batch held 3 to 13 values
   (median 7) and no batch was credited under 75 bits. `estimate_jitter_bits` gives a constant
   nothing, which is right.
2. **`init` gave up after 64 batches**, about 0.6 ms, ready or not, and
   nothing seeded the pool later.
3. **No caller looked at the refusal.** `entropy::getrandom` returns `false`
   and leaves the buffer alone when the pool is not ready. Its 18 call sites
   went on with the zeroed buffer as key, seed or nonce: the EuroCA chain and
   the attestation key, the NTS nonces, the verity salt and key, the vault
   master key and nonce, the policy-bundle key, the DNSSEC key, WASI
   `random_get`. That is why `[3d7]` and `[3d3]` failed in the same boots: two
   nonces that are both zero make a replayed answer look fresh
   (`off-path-reply-REJECTED=false`, `replay-REFUSED=false`).

**Placement is not the cause.** The kernel debug round suspected that the two
RDTSCs straddling a cache-line boundary made the difference. The diagnostic
kernel ran the loop from two assembly copies in every boot, one inside a
64-byte line and one across a boundary: in 60 boots on an idle host the
straddling copy was credited more in 35 and less in 25 (medians 3 829 and
3 892 bits over 64 batches). Where the loop lies changes the cycle count of an
iteration and with it the frequencies at which the sample is a constant, which
is how two builds of the same source can differ in how often they fail.

### The fix (`35a60c5`)

- **`crates/euroentropy`: `EntropyPool::seed_from(sources, policy)`**, the
  seeding procedure as host-testable code.
  - Order: TPM (if there is one), RDSEED and RDRAND (if CPUID reports them),
    jitter. Each is conditioned (SHA-256) into the same seed: mixed, nothing
    replaces anything.
  - Credit: TPM bytes as before (32 bytes: 256 bits). RDSEED 32 bits and
    RDRAND 16 bits per 64-bit word, and **together at most half the
    threshold**: the CPU's generator alone never opens the pool. Jitter by
    `estimate_jitter_bits`, unchanged; the threshold is 256 bits, unchanged.
  - At least four jitter batches are gathered whatever the other sources gave.
  - It gathers until the pool is ready, at most 4 096 batches at boot. At the
    bound the pool stays closed; what was gathered stays staged, and a later
    call goes on from there.
- **`kernel/src/entropy.rs`.**
  - The jitter sample now times 32 read-modify-write accesses at pseudo-random
    places of a 512 KiB block (larger than the first two cache levels of a
    core), and the duration of each sample is fed back into the walk. Measured
    in the same 60 boots as the old loop: 92 to 240 ticks per sample, 20 to
    161 different values per batch (median 37), the most frequent value 5 to
    19 % of a batch (the old loop: 46 to 92 %), and the credit over 64 batches
    is 5 713 to 6 775 bits in every boot (old loop: 1 216 to 6 170).
  - `init` prints the status line, in every image:
    `[entropy] CSPRNG seeded: 537 of 256 bits · sources mixed: tpm=none,
    rdseed=8 words, rdrand=8 words (CPU credit 128 bits, at most half the
    threshold), jitter=4 batches of 512 samples (409 bits) · 0.667 ms → OK`,
    or `[entropy] CSPRNG NOT SEEDED: … → FAILED (keys and nonces are refused;
    seeding is tried again when one is asked for)`.
  - `getrandom` is `must_use`. A request that finds the pool closed first
    waits for one more bounded seeding attempt (64 batches, at most once a
    second) and is then refused. `draw::<N>()` returns the bytes or `None`.
- **The callers.** The six self-tests draw with `draw()` and print FAILED
  without bytes; WASI `random_get` returns `EIO`; the TLS client's random and
  key share (`net::gather_entropy`, also used by the RDP client) take 32 bytes
  of the CSPRNG into what they mixed before, and no handshake starts without
  them.
- **Host tests** (`seed_tests`, 9): a quiet start is waited out; no noise ends
  at the bound with the pool closed and not a byte written; a later attempt
  seeds a pool that gave up; the CPU sources alone never open the pool;
  RDRAND is credited less than RDSEED; CPU words and jitter are mixed (the
  same jitter with and without them gives different streams); jitter is
  gathered when the TPM alone meets the threshold, and the TPM is credited
  once; a long wait does not grow the staged material; the estimator gives a
  constant nothing.
- **`[3d8]` demands what it demanded:** jitter alone, 16 batches, has to reach
  256 bits. New in it: an empty pool writes nothing into the buffer; jitter is
  in the seed of the global pool.

No health test was weakened and no threshold lowered. What changed is what is
measured, how long `init` goes on, and that a refusal is acted on.

### After

All on the final image `6fcf24c`, each boot run until `[3d3]` has printed
(`eboots.sh`; `bug065-after-kvm.txt`, `bug065-after-tcg.txt`):

| Boots | `[3d8]` / `[3d7]` / `[3d3]` FAILED | unseeded | credited bits (min / median / max) | jitter batches | `entropy::init` |
|---|---|---|---|---|---|
| before the fix, R630 KVM `-cpu host`, 204 (stopped after `[3d7]`) | 13 / 11 / not reached | **11** | jitter-alone probe 16 to 1 604, under 256 in 13 | 64 and gave up | about 0.6 ms |
| R630, KVM, `-cpu host`, **204** | **0 / 0 / 0** | **0** | 488 / 544 / 578 (128 of them RDSEED and RDRAND, 360 to 450 jitter) | 4 in every boot | 0.58 / 0.75 / 1.51 ms |
| R630, KVM, `-cpu qemu64,-rdrand,-rdseed` (jitter only), **20** | **0 / 0 / 0** | **0** | 380 / 413 / 439 | 4 | 0.64 / 0.73 / 0.93 ms |
| build server, TCG, `-cpu qemu64` (jitter only), **20** | **0 / 0 / 0** | **0** | 416 / 428 / 441 | 4 | 4.0 / 5.0 / 9.0 ms |

The jitter-alone probe of `[3d8]` (16 batches, wants 256 bits) read 1 485 to
1 778 bits in the 204 boots; before the fix 16 to 1 604.

Also seeded, with the line in the log: the ten TCG matrix boots, the four
megaraid boots, the soak guest, and the public image in every run of
section 5 (`[entropy] CSPRNG seeded` under KVM with RDSEED and RDRAND, under
TCG from jitter alone: 440 of 256 bits).

First round (image of `c0db228`, before BUG-083/084):
174 KVM boots and 20 jitter-only boots, 0 FAILED, 0 unseeded
(`bug065-after-kvm-first-round.txt`).

Seen in the matrix: with `net.driver=kernel` the `[entropy]` line is printed
twice. Something asks for random bytes before `entropy::init` runs; the
request seeds a pool itself (3 batches, 309 and 321 bits) instead of getting
weak bytes, and `init` then seeds the pool it keeps. Which caller it is was
not traced.

### What it costs

- `entropy::init` itself: 0.58 to 1.51 ms under KVM (median 0.75 ms; the old one
  took about 0.6 ms when it ran its 64 batches and less when it was ready
  sooner), 4 to 9 ms under TCG. It allocates and frees one 512 KiB block.
- Kernel entry to desktop of the public image by `[boot-time]`, R630, five
  boots each, alternating on the same two CPUs: before the fix (`b121e06`)
  776 to 857 ms, median 838; final 791 to 853 ms, median 830; base `7858b72`
  756 to 830 ms, median 821.
- `scripts/boot-time.py`, QEMU start to desktop, the same boots
  (`kvm-ab.txt`): final **2.62 s** (2.33 to 2.81), before the fix 2.77 s (2.67
  to 2.96), base 2.67 s (2.38 to 2.82); modern virtio only: 2.40 s against
  2.42 s. The R630 ran the soak and the boot loops beside it; the differences
  are inside the spread. The previous proofs had 2.70 and 2.71 s.

### Left open (new entries in the tracker)

- **BUG-080** (HIGH): `getrandom`, `/dev/urandom` and `/dev/random` of the Linux
  ABI are served by `ring3::fill_random`, splitmix64 over a counter and the
  tick count, not by the CSPRNG. Found by reading; not changed here, because
  it is on the path of every glibc process and wants its own test round.
- **BUG-081** (MEDIUM): `euroid::rand_bytes` and `net::rand_u64` fall back to a
  tick-seeded xorshift and to `rdtsc ^ hpet ^ counter` on a machine without a
  TPM, respectively without RDRAND.
- **BUG-082** (MEDIUM): `estimate_jitter_bits` is a heuristic. It credited the
  old loop's near-periodic series 60 to 95 bits per batch. The new sample is
  not such a series, but SP 800-90B health tests on the raw samples and an
  off-line min-entropy measurement of the walk are still to do.

## 3. The remaining points

### BUG-063 from the desktop Terminal

The kernel debug round read from the code that a foreground program started in
the Terminal while a userspace driver polls was exposed, and did not try it.

- **It was real.** `bug063.txt` (a `probe.py` scenario): `hello` typed in the
  Terminal, `ping 10.0.2.2`, `ps`, `hello` again, on a guest with an e1000e NIC
  served by `e1000d` (`bug063-desktop.txt`, R630, KVM): the public image of `7858b72` dies in **5 of 5** runs with `[idt] PAGE FAULT
  addr=0xfffffffffffffff5 … @ 0x0` before the Terminal has printed the
  program's line; the final image runs it in 5 of 5 (both `hello` lines, the
  ping answered, no fault line).
- **`gui-smoke.py` step** "Terminal: after a foreground program, commands that
  reach the process table and a userspace driver (BUG-063)": `/bin/hello`,
  `ps`, with `--net e1000e` a `ping` of the SLIRP gateway that has to be
  answered through `e1000d`, two idle seconds, `hello` again and an `echo`.
  `--net e1000e` is new: an e1000e NIC on SLIRP with `restrict=on`.
  OK in every run of this proof: TCG without NIC and with `--net e1000e`
  (`ping 10.0.2.2 through e1000d: echo-reply OK`), KVM with `--net e1000e`,
  and the two smoke passes before the monkey soaks.

### The drag step

`gui-smoke.py` dragged a window and looked once; under TCG on a busy host the
guest latched the press when the pointer was most of the way, and the window
had followed for 37 % of the distance where the step wants half. The step now
waits for the guest twice (`drag_window`): after the press the pointer moves
six pixels and stays there until a screenshot shows the window displaced, and
at the end the button stays down until a screenshot shows the window arrived.
The demand is the same (at least half of the way, not past it). In the
two TCG runs of this round, beside the matrix and the q35 legs at a host load
of 12 to 17, all eleven drags landed on the pixel (70, 50 of 70, 50), as in
the two runs of the first round. Not shown: a run in which the old step would
have failed at that moment.

### Help texts and the shortcut overview

Read against the merged code, and looked at on the screenshots of the help
panel at both sizes (section 6): the overview names the mouse wheel (pointer
table: help panel, EuroBeheer, EuroFiles, EuroText, Terminal; the footer of
the panel: "PgDn, the wheel or click the text"), AltGr has its row and its
note (third level on the Belgian, French and German layout; not Alt), and Esc
lists the context menu and the notification list. The desktop pass had made
these changes; the kernel debug round changed nothing a user sees, and neither
does BUG-065. No help text was changed in the integration.

## 4. BUG-083 and BUG-084, found by the CI q35 matrix

The first q35 matrix of this round was 11 of 13
(`q35-matrix-first-round.txt`): `usbhub` ("typed uname never echoed") and
`hwprobe` ("hwprobe output not seen"). Both type a command on the desktop of
the self-test image. The same two legs on the heads of the two branches
(`q35-legs-by-branch.txt`): `feature/kdebug` passes both, `feature/uidebug`
fails both. The desktop pass had not run this matrix.

- **BUG-083.** `ps2::altgr_selftest` emptied the scancode ring after its
  split-prefix case without decoding the release of AltGr. The AltGr latch
  stayed set for the rest of the boot: `uname` typed afterwards arrived as
  `€` (`[e2e] $ €`, `typed-on-the-selftest-desktop.txt`). Fix: the rest of the
  ring is decoded, and the test fails unless every latch is released at its end.
- **BUG-084.** The X11 self-tests stage mouse input as three bare bytes per
  PS/2 packet. Since the wheel support a mouse that answers as an IntelliMouse
  sends four, and the staged bytes were read as pieces of other packets. The
  press those tests leave behind, which puts the Terminal in front at the
  first iteration of the desktop loop, was lost (the empty GTK demo window
  kept the keyboard), and a wheel movement nobody made scrolled the Terminal
  63 lines back. Fix: `mouse::push_packet` sends a whole packet in the format
  the mouse is in.

Both are self-test code and self-test image only; the public image runs no
self-test and types correctly (`gui-smoke.py`, every run). After the fix:
13 of 13 (`q35-matrix.txt`); `uname` typed 8 s after the desktop runs (`typed-on-the-selftest-desktop.txt`).

## 5. Tests

### Build server (QEMU 10.2, TCG)

| Test | Result |
|---|---|
| `cargo test` (default members) | 187 suites, 1 330 passed, 0 failed, 1 ignored (`hosttests.txt`; 1 312 and 1 313 on the two branches: their 8 + 9 new tests and the 9 of `euroentropy`) |
| Self-test image, TCG matrix: `user-smp{4,1}-{iommu,noiommu}`, `kernel-smp{4,1}`, `storage-smp{4,1}-iommu-1g`, `combined-smp{4,1}-iommu-1g` | 10 of 10 to the desktop, no fault line, no NMI probe. `markers-compare.py` against `../integ-2026-11/`, `../kdebug-2026-11/` and `../uidebug-2026-11/` (`comparison-tcg.txt`, 30 comparisons): **0 worse, 0 changed** in all. New against the first: `[altgr]`, `[bug053]`, `[bug063]`, `[disks]`, `[entropy]`, `[fdspace]`, all OK. Kernel entry to desktop 69 to 86 s at 512 MiB, 247 to 324 s at 1 GiB (`tcg-summary.txt`) |
| `[lk]` | `boot-total order inversions=0 (self-test provoked=2) task-lock-with-IF0=0` in all ten |
| CI q35 matrix, 13 legs | **13/13** (`q35-matrix.txt`); first round 11/13: BUG-083, BUG-084 |
| `megaraid-proof.sh`, with IOMMU and with `NO_IOMMU=1` (4 boots) | 0 markers worse than `../integ-2026-11/` and `../kdebug-2026-11/` (`comparison-mr.txt`, 8 comparisons); host checks 4/4 in both |
| `scripts/repro-build.sh`, the CI procedure (build, remove the `.efi`, touch `main.rs`, build) | **REPRODUCIBLE**: kernel 62f23d76… and loader 2a741656… both times, 0 paths of the build machine (`repro-build.txt`) |
| Public image, `gui-smoke.py` under TCG (usb-kbd + usb-tablet, smp 2, 2 GiB), twice | **PASS 67/67** without NIC (`gui-smoke-tcg.txt`) and **PASS 67/67** with `--net e1000e` (`gui-smoke-tcg-e1000e.txt`): the 66 of before and the new step. First round: 67/67 twice |
| Public image, `wizard-install-test.py` | **15/15** (`wizard-install-test.txt`); the installed disk booted again with `display.mode=1280x720`: 7/7 |
| Public image on open user-net (`boot-time.py --net open`), virtio-net and e1000e | lease, `ping 10.0.2.2` OK, `nslookup`, `fetch example.com` HTTP 200, `https example.com` TLS 1.3 HTTP 200, on both NICs (`network-open.txt`) |
| No self-test in the public image | 0 lines of `[bug063]`, `[fdspace]`, `[bug053]`, `[ag-lock]`, `[altgr]`, `[3d8]`, `[3d7]`, `[3d3]`; one `[entropy]` line (`public-image-selftest-lines.txt`) |
| 20 entropy boots under TCG | section 2 |

### Dell R630 (KVM)

Only built images and test scripts went there (`/srv/euroos/integ2-runs`); no
source tree, no key. Every QEMU with `-nic none` or SLIRP `restrict=on`,
pinned; no VFIO, no PCI device touched.

| Test | Result |
|---|---|
| 204 + 20 entropy boots | section 2 |
| `scripts/boot-time.py`, public image, 5 boots (SATA medium, smp 2, 2 GiB, `restrict=on`) | median **2.62 s** (2.33 to 2.81); section 2, "What it costs" |
| Monkey soak 35 min, USB input, smp 4, 4 GiB, no NIC, seed 20261112 | PASS: smoke 67/67, then 2 225 actions (50 wheel, 20 AltGr), every check ok, no crash signature, console max 450 ms |
| Monkey soak 35 min, virtio input, smp 2, 2 GiB, `restrict=on`, seed 20261113 | PASS: smoke 67/67, then 2 102 actions (53 wheel, 18 AltGr), every check ok, no crash signature, console max 2 155 ms, one echo (2 054 ms in the desktop pass, 250 ms in this proof's first round) (`gui-soak-virtio.txt`) |
| `gui-smoke.py`, KVM, `--net e1000e` | PASS 67/67 (`gui-gs-kvm-e1000e.txt`) |
| BUG-063 scenario from the desktop, base against final | 5 of 5 crash, 5 of 5 run (`bug063-desktop.txt`) |
| NVMe: the public image booted beside a 64 MiB NVMe disk of random data | sha256 before and after equal, `NVME-UNTOUCHED` (`nvme-check.txt`) |
| Lock screen frame of the self-test image | 0 pixels off the kernel debug round's and the base's (section 1) |
| Soak, 2 h 05 | below |

### Soak, 2 h 05

`loadtest.py --kvm --smp 4 --nic e1000e --restrict --nmi-on-hang --mem 16G
--disks 8G --host-http --hostfwd … --hammer 4 --procs 32 --scenario soak
--soak-churn-proc 8 --duration 7500`, self-test image, `net.driver=user`.

| | first round (image of `c0db228`) | final image (`r630-soak*`) | kernel debug round |
|---|---|---|---|
| commands | 2 650, all ok, `completed clean` (`r630-soak-first-round-summary.txt`) | **2 744, all ok**, `completed clean` | 2 671 |
| `churn proc` commands / glibc launches | 339 / 2 735 | 351 / 2 831 | 342 / 2 758 |
| `heap_used` first / at 10 min / last | 149 714 256 / 148 555 424 / 148 580 512 | 149 714 768 / 149 814 608 / 148 580 512 | |
| slope after 10 min (least squares) | +0.17 kB/min (range 1 285 kB) | **+0.17 kB/min** (range 1 285 kB) | +0.17 kB/min (1 282 kB) |
| tasks / files / kernel stacks / process pool | the same | 56 / 187 / 0 / 887 296, flat | the same |
| httpd during the soak | 889 999 answered, 0 failed, p50 5.1 ms, p99 124 ms | 917 858 answered, 0 failed, p50 4.9 ms, p99 123 ms | 883 286, 10 failed |
| serial | 0 panics, 0 fault lines, 0 NMI probes | 0 panics, 0 fault lines, 0 NMI probes, 0 refused frame frees | the same |
| boot of the guest against the kernel debug round's soak boot | not compared | 0 worse, 0 changed; new `[altgr]`, `[disks]`, `[entropy]`, OK (`comparison-r630-soak-boot.txt`) | |

One FAILED line after the boot, in both soaks: `[3g2-wire] WATCHDOG TRIPPED -
main loop hung; would reset`. It is the deadman of the desktop loop (15 s)
during the first `churn fs 1`, a shell command that takes 30 to 57 s. The same
line, at the same command, is in the serial logs of the integration's soak
and of both soaks of the kernel debug round, which are still on the R630
(`watchdog-line-in-soaks.txt`); it was not seen before because the markers of
a soak were cut at the desktop. Not a regression; BUG-085.

## 6. Screenshots (public image, looked at)

`screenshots/1080/` and `screenshots/720/` (`display.mode=1280x720`):

- `00-desktop-boot.png`, `01-desktop-empty.png`: the desktop as it starts, and
  empty. No seam in the background; at 1280x720 the Terminal lies between the
  dock and the status panel.
- `lock-installed.png`: the lock screen of the system installed by the wizard
  test, booted from its disk (at 720 with `display.mode` on the installed
  ESP). Whole gradient, the card in the middle, user `tester`, no auto-login.
- `20-eurobeheer-storage.png`: "1.7 MiB of 8.0 MiB used (21%)", `sata0  256 MiB,
  SATA (AHCI), QEMU HARDDISK`, the integrity check; nothing outside the window.
- `30-help-overview.png`, `31-…-wheel.png`, `32-help-shortcuts.png`,
  `33-…-page2.png`, `37-help-terminal.png`: the help panel. The shortcut page
  has the Esc row with the context menu and the notification list, the AltGr
  row and note, and the footer "PgDn, the wheel or click the text". At 720 the
  panel fits the screen and lies over the status panel.
- `screenshots/lock-integ-kvm.png`, `lock-integ-tcg.png`: the self-test frames
  of section 1.

## Open

- **BUG-080** (HIGH), **BUG-081**, **BUG-082**: randomness outside the CSPRNG and
  the estimator, section 2. BUG-065 closes the pool and its callers; a
  userspace program still gets its `getrandom` bytes from a counter.
- **BUG-085** (LOW): the desktop loop's deadman trips on any shell command
  longer than 15 s and prints a FAILED line; in every soak since 2026-11-10.
- Which caller asks for random bytes before `entropy::init` with
  `net.driver=kernel` was not traced (it is served correctly).
- The R630 has RDSEED and RDRAND and no TPM; a boot with a TPM (the pool then
  takes its 32 bytes too) was run under no configuration of this proof. A CPU
  with RDRAND and without RDSEED was not run either; host tests cover the credit.
- The bound of `init` (4 096 batches without reaching 256 bits) was reached in
  no boot; the closed pool and the refusals are shown by host tests only.
- The old drag step was not caught failing in this round, so the new one is
  shown passing, not shown curing a failure.
- The BUG-063 step of the two monkey soaks ran without the driver (their
  configurations have no e1000e NIC); the driver path ran in three other runs.
- `repro-build.sh` on one machine and one checkout path, as before.
- Everything the two branch proofs list as open (fd spaces of the glibc
  group, PS/2 devices by injection, scroll bars, dead keys, …).

## Cleaned up

- Build server: `target/` of the worktree, every built image and kernel in
  `/opt/euroos/integ2-runs`, the QEMU work files, the throwaway worktree
  `/opt/euroos/wt-integ2-x` (base, pre-fix and branch-head builds) and both
  symlinks to the dev key are removed; serial logs and run outputs stay in
  `/opt/euroos/integ2-runs`. The main checkout and the two branch worktrees
  were read only.
- R630: images, disk files and work copies under `/srv/euroos/integ2-runs` are
  removed; the run outputs stay there. Every QEMU was stopped by its own PID
  (checked non-empty and above 1). For about 75 s forty `nice 19` busy loops
  ran there to load the CPUs for the diagnosis; no setting was changed, no
  PCI device touched.
- Two diagnostic kernels (raw sample dumps) and one with debug prints for the
  keyboard focus were built from uncommitted changes and are gone; the first
  one's code is `entdiag.rs.txt`.
