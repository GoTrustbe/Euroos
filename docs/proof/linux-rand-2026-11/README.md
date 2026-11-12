# The random bytes of Linux-ABI programs and of the kernel's own callers (BUG-080, BUG-081), 2026-11-12

Branch `feature/linux-rand` on `feature/app-control` `b9a5755`. Not merged, not
pushed. Bugs: `../../LOADTEST-TRACKER.md` (BUG-080 and BUG-081 fixed; BUG-086
to BUG-089 new; BUG-082 unchanged).

| Commit | What |
|---|---|
| `91afd7d` | `userland/randprobe.c` (musl and glibc), the `[linux-rand]` boot self-test, `scripts/rand-boots.py`. On this commit the self-test prints FAILED: BUG-080, measured |
| `0b33035` | one seeding of the CSPRNG, early in the boot; one `[entropy]` line; the pool can be asked from any context and waited for |
| `f83007a` | **BUG-080**: `getrandom`, `/dev/urandom`, `/dev/random`, `AT_RANDOM` and the ASLR slides come from the CSPRNG |
| `fd0982b` | **BUG-081**: `euroid::rand_bytes`, `net::rand_u64`, the seeds of the boot self-tests, the vault nonce, the TPM session nonce, `mktemp` |
| (last) | this proof, the tracker, `SPRINT-PLAN-PHASE3.md` |

**Which image was tested.** Everything below ran on the images of `fd0982b`
(the commit after it changes documents only): self-test image sha256
`0b290edbaf5e96ef…`, public image `734c693bae5d69e0…`. "Before" is the
self-test image of `91afd7d` (`39fef28f3a6a4a8d…`: the base plus the test, no
fix) and the public image of `b9a5755` (`65371140e77b11c1…`). Serial logs stay
on the build servers (`*.log` is not committed).

## 1. What was wrong

Found by reading and by the grep round the task asked for (splitmix,
xorshift, LCG, `rand_u64`, fixed seeds, `rdtsc`, tick counts and RTC seconds
used as seeds, `tpm::get_random` with a fallback), then measured with the
probe on the image before the fix.

| Source | Who gets it | Before | Now |
|---|---|---|---|
| `getrandom` syscall (318, both dispatchers) | every glibc and musl program: NSS and BoringSSL in Chromium, `arc4random`, hash seeds, `mkstemp` | splitmix64 over a call counter plus `ticks * constant`. The first 8 bytes of a call are the state: `rand-boots.py` predicts words 2 to 4 of every call from word 1 (3 of 3, all 16 calls of the two boots) | HMAC-DRBG of the kernel pool, in 4 KiB pieces |
| `getrandom` flags | | any value accepted, never refused | unknown flag and `GRND_RANDOM\|GRND_INSECURE`: `EINVAL`; pool closed: wait, or `EAGAIN` with `GRND_NONBLOCK` |
| `/dev/urandom`, `/dev/random` (read) | NSS's software token, OpenSSL, anything that opens the device | the same splitmix64 | the CSPRNG; a read waits for the seed |
| `/dev/urandom`, `/dev/random` (pread) | | the placeholder bytes the device was registered with: `00bb7733efab6622…` for `/dev/urandom` at offset 0 in every process of every boot, 32 zero bytes for `/dev/random` | the CSPRNG (a device has no offsets) |
| `/dev/urandom` registered twice | a chrome run (`main.rs` registers it again as an ordinary 4 KiB file) | whichever entry came first in the file table was opened; one of them was a fixed pattern that ends after 4 096 bytes | one entry per device, no stored bytes |
| `AT_RANDOM` (three stack builders) | glibc and musl: the stack guard and the pointer guard | the constant `5a45640726c1e083a24d6c0f2ec9e88b`: 8 of 8 processes in two boots. glibc's guard was `0045640726c1e083`, its pointer guard `a24d6c0f2ec9e88b`, musl's guard `5a00640726c1e083`, in every process of every boot | 16 bytes of the CSPRNG per process image |
| `aslr_slides` | every Linux-ABI process | `net::rand_u64` (below) | 8 bytes of the CSPRNG |
| `net::rand_u64` | DNS transaction id and source port, TCP initial sequence numbers (four places), ephemeral ports | RDRAND alone; without RDRAND `rdtsc ^ hpet ^ counter` | the CSPRNG |
| `euroid::rand_bytes` | password salts (13 places), the session id of a login, the wallet's issuer and holder seeds | the TPM; without one a xorshift seeded with the tick count | the CSPRNG |
| the vault master key (`main.rs`) | `vault::get` for the running system | the TPM; without one `a5a5…a5` | the TPM; without one the CSPRNG |
| FDE key, VPN seeds, ML-KEM seed and randomness, EuroCA, attestation and EuroIDM seeds (`main.rs`) | the boot self-tests; what they leave behind (the CA fingerprint, the VPN and IDM public keys the shell shows) | fixed bytes without a TPM (`5a…`, `11…` to `14…`, `21…`, `22…`, `3c…`, `2a…`/`9e…`, `1d…`) | the CSPRNG |
| vault seal nonce | | the TPM; without one a counter | the CSPRNG |
| caller nonce of the two TPM policy sessions | | `5a…` when `GetRandom` failed | the CSPRNG |
| `mktemp` of the shell | | `epoch ^ ticks * constant` | the CSPRNG |

Not there, so nothing to fix: a vDSO `getrandom` (`vdso.so` exports
`clock_gettime`, `gettimeofday` and `time`), `/proc/sys/kernel/random/*`.
Already on the CSPRNG since BUG-065: the TLS client random and key share, NTS,
verity, attestation (`attest3`), the policy bundles, `gdpr`, WASI `random_get`.
Deterministic on purpose and left alone: the fuzzers (`eurofuzz`, `gfuzz`),
the shuffle of `euromusic` and `eurocoreutils`, test vectors.

### The order of the boot, and the second `[entropy]` line

`entropy::init` was at line 2304 of `main.rs`: after the scheduler had
started, after the network bring-up, after `euroid::init_from_disk`, and
after the boot programs had run. "The pool is seeded before the first process
starts" (the tracker's note on BUG-080) was not so.

With `net.driver=kernel` the bring-up of the self-test image fetches a page
over HTTPS before that point. `net::https_get` asks `gather_entropy` for the
key share, which draws 32 bytes: the request found no pool, seeded one itself
(`SeedPolicy::RETRY`, three batches) and printed the status line; `init` then
seeded a second pool and printed it again. In the serial log of the image
before the fix the first line stands between the FIN of the HTTP connection
and the FIN of the HTTPS one (`entropy-line-twice-before.txt`):

```
[tcp] @0 FIN from 172.66.147.243:80 to sport 55277 — closing
[entropy] CSPRNG seeded: 302 of 256 bits · … jitter=3 batches of 512 samples (302 bits) · 3.752 ms → OK
[tcp] @0 FIN from 172.66.147.243:443 to sport 61809 — closing
…
[tpm] no TPM @ 0xfed40000 (intf-id=0x00000000, DID_VID=0x00000000)
[entropy] CSPRNG seeded: 406 of 256 bits · … jitter=4 batches of 512 samples (406 bits) · 6.119 ms → OK
```

It was served correctly both times; what was wrong is that the generator had
two starts and that everything before line 2304 depended on a caller seeding
it by accident.

## 2. The fix

- **One seeding, early** (`0b33035`). `tpm::init` and `entropy::init` run right
  after the HPET is up: before `euroid::init_from_disk`, the network and every
  process. `init` does its work once; a request that comes before it does the
  same seeding and prints `[entropy] random bytes were asked for before the
  boot seeded the CSPRNG: seeding now` (seen in one configuration, see Open). The
  3D-8 self-test stays where it was and proves the pool that is there.
- **The pool can be asked from anywhere.** It is taken with interrupts off
  (`sched::IfOffGuard`): `net::alloc_port` draws with interrupts off, and a
  holder parked by the timer would hang it. A retry gathers its noise with the
  pool out of the lock.
- **Callers that cannot refuse wait.** `entropy::fill_wait` and `draw_wait`
  keep gathering noise (64 batches a round) until the pool opens and return
  nothing else. `AT_RANDOM`, the slides, `net::rand_u64`, `euroid::rand_bytes`
  and the seeds above use them. `getrandom()` and `draw()` still refuse, for
  the callers that can.
- **The Linux ABI** (`f83007a`), as Linux does it:
  - `getrandom(buf, len, 0)` waits until the pool is seeded;
    `GRND_NONBLOCK` returns `-EAGAIN` and writes nothing.
  - `GRND_RANDOM` is the same generator. `GRND_INSECURE` is accepted and
    served like no flag: Linux would hand out bytes of an unseeded generator
    there, this kernel has none to give. That is the one deliberate
    difference.
  - Unknown flags and `GRND_RANDOM|GRND_INSECURE` are `-EINVAL`, a bad pointer
    `-EFAULT`, a length of 0 returns 0.
  - A request is never cut short; one call returns at most 32 MiB − 1.
  - A read of `/dev/urandom` or `/dev/random` waits for the seed (Linux's
    `/dev/urandom` would not; here it never returns unseeded bytes).
    `O_NONBLOCK` on the device is not looked at.
  - The wait does not give the CPU away: a round is a few milliseconds of
    noise gathering. A first version yielded between rounds; the foreground
    musl run of the self-test then died with exit 139 (see Open).

## 3. Before and after, the same test

`randprobe` (one C source, `musl-gcc -static-pie -Os` and `gcc -O2` against
glibc) prints `AT_RANDOM`, the stack guard its libc made of it, `getrandom`
with each flag, both devices by `read` and `pread`, the short-read rules
(16 × 256 bytes and 7 smaller requests), and with `stat` the bit balance, the
chi-square of the byte histogram and a search for repeated 16-byte blocks over
a stream from `getrandom` and as much from `/dev/urandom`. The `[linux-rand]`
self-test runs it as two musl and two glibc processes and compares them; a
fifth run meets a pool the syscall layer reports closed.
`scripts/rand-boots.py` compares the same lines between boots.

| | before (`91afd7d`) | after (`fd0982b`) |
|---|---|---|
| `[linux-rand]` verdict | FAILED in every boot (2 TCG, 2 KVM) | OK in every boot (2 TCG, 3 KVM, 2 KVM without RDRAND and RDSEED, the 10 of the matrix, 104 short boots) |
| `AT_RANDOM`, two boots × four processes (TCG) | 8 values, **1 distinct**: `5a45640726c1e083a24d6c0f2ec9e88b` | 8 values, 8 distinct |
| stack guard, same | 8 values, 2 distinct (one per libc) | 8 values, 8 distinct |
| `getrandom`, `/dev/urandom`, `/dev/random`: words 2 to 4 of a 32-byte result predicted from word 1 | **32 of 32** results (TCG), 32 of 32 (KVM) | 0 |
| two `pread`s at offset 0 | equal in every process, and equal in every boot | differ |
| unknown flag `0x100`; `GRND_RANDOM\|GRND_INSECURE` | returned 32 bytes | `-1`, `EINVAL` |
| pool reported closed, `GRND_NONBLOCK` | returned 32 bytes | `-1`, `EAGAIN`, buffer untouched |
| pool reported closed, no flags | returned at once | returned 32 bytes after three rounds of waiting |
| 1 MiB from `getrandom`, 1 MiB from `/dev/urandom` (glibc run): ones of 8 388 608 bits, chi-square (255 degrees of freedom), repeated 16-byte blocks | 4 193 155 and 4 192 313, 245 and 259, 0 and 0 | 4 192 800 and 4 196 451, 263 and 275, 0 and 0 (KVM boot 1) |
| `[entropy]` status lines with `net.driver=kernel` (TCG) | 2 | 1 |

The statistics do not tell the two apart, and are not meant to: splitmix64
is statistically clean. They would catch a generator that is broken in the
plain sense (stuck bits, a short period, the 4 KiB pattern). What tells them
apart is the constant, the equal values and the prediction.

The values of every process of every boot are in `before-tcg.txt`,
`after-tcg.txt`, `before-kvm.txt`, `after-kvm.txt` and
`after-kvm-jitter-only.txt`. Over the 104 short KVM boots, four processes
each: 416 `AT_RANDOM` values, 416 stack guards, 416 `getrandom`
results and as many from each device, no value twice (`r630-short-boots.txt`).

At 512 MiB the glibc runs of the self-test do not start ("no arena for
glibc", as for the other glibc self-tests at that size); the verdict then
rests on the two musl processes and says so. The 1 GiB legs of the matrix and
every KVM boot run all four.

The closed pool of the fifth run is a switch in the syscall layer
(`ring3::RANDOM_ST_CLOSED`): it exercises the `EAGAIN` and wait paths of
`getrandom` with the real pool untouched, so that the process can still be
started. The pool's own refusal is what `[3d8]` and the nine host tests of
`euroentropy` prove.

## 4. Tests

### Build server (QEMU 10.2, TCG)

| Test | Result |
|---|---|
| `cargo test` (default members) | 187 suites, 1 330 passed, 0 failed, 1 ignored (`hosttests.txt`); the same counts as the base: this round adds no host test, its tests are the probe and the boot self-test |
| Self-test image, TCG matrix: `user-smp{4,1}-{iommu,noiommu}`, `kernel-smp{4,1}`, `storage-smp{4,1}-iommu-1g`, `combined-smp{4,1}-iommu-1g` (`tcg-boot.sh`, `tcg-matrix.sh` of `../integ-2026-11b`) | 10 of 10 to the desktop, no fault line, no NMI probe. `markers-compare.py` against `../integ-2026-11b/` (`comparison-tcg.txt`, 10 comparisons): **0 worse, 0 changed**, nothing gone; new in each: `[linux-rand]` OK. `[lk] boot-total order inversions=0 (self-test provoked=2) task-lock-with-IF0=0` in all ten. One `[entropy]` line in all ten (the two `kernel` legs had two). Kernel entry to desktop 68 to 79 s at 512 MiB (base 69 to 86), 245 to 332 s at 1 GiB (base 247 to 324) (`tcg-summary.txt`) |
| `[linux-rand]` in the matrix | OK in 10 of 10; the six 512 MiB legs with two musl processes ("glibc not run: (no arena for glibc)"), the four 1 GiB legs with two musl and two glibc processes (`tcg-newtests.txt`) |
| CI q35 matrix (`run-metal-matrix.py`, 13 legs: base, nvme, ahci, e1000e, hda, usb, usbhub, usbaudio, usbnet, scan, printer, power, hwprobe) | **13 of 13** PASS (`q35-matrix.txt`) |
| `scripts/repro-build.sh`, twice, the second after removing the `.efi` and touching `main.rs` | **REPRODUCIBLE**: kernel `aec2bd657d53…`, loader `a6b8a3b0ab39…` both times; 0 paths of the build machine in the kernel (`repro-build.txt`) |
| Before and after, two boots each, 1 GiB, smp 4 (`rand-boots.py`) | section 3 (`before-tcg.txt`, `after-tcg.txt`) |
| Public image on the open network, virtio-net and e1000e: `net`, `ping`, `nslookup`, `fetch`, `https example.com`, `netstat` | all answered on both; TLS 1.3 handshake and 888 bytes over https on both; one `[entropy]` line, no self-test line in the public image (`network-open.txt`) |
| One chrome-boot run each, base and final, side by side, 40 minutes (`chrome-ab.sh`, `chrome-pack2.img`) | no difference: the same number of serial lines per tag (2 400 lines), chrome connects to the X server and sends 183 requests in both, no fault line in either. Neither reached a paint or `DONE` inside the 40 minutes, so this is not a chrome start (`chrome-ab.txt`) |

### Dell R630 (KVM, `-cpu host`: RDSEED and RDRAND, no TPM)

| Test | Result |
|---|---|
| 104 short boots of the self-test image, each to the `[linux-rand]` verdict (`short-boots.sh`: 2 GiB, smp 2, no NIC; four lanes) | `[3d8]`, `[3d7]`, `[3d3]` **FAILED in 0**; **unseeded 0**: one `[entropy] CSPRNG seeded` line in 104 of 104; `[linux-rand]` OK in 104; no crash line. Credited bits 491 to 565 (median 539), four jitter batches in every boot, `entropy::init` 0.59 to 1.35 ms (median 0.78) (`r630-short-boots.txt`) |
| Before and after under KVM (`rand-boots.py`): 2 boots before, 3 after, 2 after with `-cpu qemu64,-rdrand,-rdseed` (jitter only) | before: FAILED, one `AT_RANDOM` for 8 processes, 32 of 32 results predicted; after: PASS in all 5, 12 + 8 values of each kind, all distinct (`before-kvm.txt`, `after-kvm.txt`, `after-kvm-jitter-only.txt`) |
| Two full boots with the `smoke` scenario (4 GiB, smp 4, e1000e `restrict=on`; one with `intel-iommu`) | desktop, `completed clean`, `[3d8]`/`[3d7]`/`[3d3]`/`[linux-rand]` OK, one `[entropy]` line (`r630-full-boots.txt`). The planned 100 boots of this kind were replaced by the short ones after these two: a boot with the smoke pass takes over three minutes |
| Boot of the soak guest against `../integ-2026-11b/r630-soak-boot.markers` | 0 worse, 0 changed, nothing gone; new: `[linux-rand]` OK (`comparison-r630-soak-boot.txt`) |
| Public image, GUI smoke, no NIC (`gui-smoke.py --kvm --smp 4 --mem 4G`) | **PASS, 67/67** (`gui-smoke-kvm.txt`) |
| Public image, GUI smoke, e1000e `restrict=on` (the userspace driver) | **PASS, 67/67** (`gui-smoke-kvm-e1000e.txt`) |
| Boot time of the public image (`boot-time.py`, base and final alternating) | section 5 |

FAILED lines that are not new. Under KVM the self-test image prints
`[fuzz] … kernel-survived=false (exit 0) → FAILED` in every boot, before the
fix and after it; it is in the base markers of the R630 as well
(`../integ-2026-11b/r630-soak-boot.markers`) and passes under TCG. With
`restrict=on` the three network self-tests that need a route out fail, as in
the base markers. Neither was looked into here.

### Soak, 1 h

`loadtest.py --kvm --smp 4 --pin 4-7 --nic e1000e --restrict --nmi-on-hang
--mem 16G --disks 8G --host-http --hostfwd 18591 --hammer 4 --procs 32
--scenario soak --soak-churn-proc 8 --duration 3600`, self-test image,
`net.driver=user`. Compared with the first hour of the previous soak
(`../integ-2026-11b/r630-soak.csv`, image of `6fcf24c`; `soak_compare.py`).

| | this round (`fd0982b`) | previous soak, first hour | previous, whole 2 h 05 |
|---|---|---|---|
| commands | **1 364, all ok**, `completed clean` | 1 245, all ok | 2 744, all ok |
| commands per hour | 1 366 | 1 246 | 1 314 |
| `churn proc 8` | 175 commands, median 5 968 ms | 159, median 6 238 ms | 351, median 6 354 ms |
| `spawn worker 32` | 131, median 1 872 ms | 120, median 1 956 ms | 263, median 1 755 ms |
| glibc launches (`GLIBC-DYNAMIC-OK`) | 1 423 | | 2 831 (1 356 per hour) |
| `[churn-proc]` verdict lines | 1 012, 0 FAILED | | 2 024, 0 FAILED |
| panics, ring-0 faults, NMI probes | 0 | | 0 |
| httpd during the soak (4 clients) | 440 088 answered, 0 failed, p50 5.0 ms, p99 124 ms | | 917 858, 0 failed, p50 4.9 ms, p99 123 ms |
| FAILED lines after the desktop | one: `[3g2-wire] WATCHDOG TRIPPED` (BUG-085, in every soak) | | the same one |

Every launch of these 1 423 drew its `AT_RANDOM` and its slides from the pool;
the httpd connections drew their sequence numbers there. No request was
refused and no caller waited (`r630-soak-summary.txt`).

## 5. What the CSPRNG costs on the process start path

Three requests per process start: 16 bytes for `AT_RANDOM` and two calls of
`aslr_slides` (8 bytes each). glibc may add a `getrandom` of its own (the
tcache key, at the first `malloc`). Each request is one `HMAC_DRBG_Generate` under the pool
lock with interrupts off. It was not timed on its own inside the kernel; what
was measured is whether anything that starts processes got slower.

**The same commands before and after, alternating** (`r630-ab.sh`: self-test
image of `91afd7d` and of `fd0982b`, KVM, smp 4, 4 GiB, CPUs 12-15; per boot
40 × `churn proc 8`, which starts and reaps 8 glibc processes, and
10 × `spawn worker 32`, 32 musl background processes; three rounds;
`r630-process-start-ab.txt`):

| | before, median per command | after, median per command |
|---|---|---|
| `churn proc 8`, round 1 / 2 / 3 | 5 044 / 4 964 / 5 004 ms | 5 165 / 5 024 / 5 006 ms |
| `churn proc 8`, 120 commands each | 4 994 ms (mean 5 036) | 5 044 ms (mean 5 152) |
| `spawn worker 32`, round 1 / 2 / 3 | 1 551 / 1 550 / 1 557 ms | 1 507 / 1 551 / 1 616 ms |
| `spawn worker 32`, 30 commands each | 1 551 ms (mean 1 569) | 1 551 ms (mean 1 586) |

`churn proc 8`: +1.0 % on the median over all three rounds, +2.4 %, +1.2 % and
0.0 % per round; the three "before" medians alone span 1.6 %. The host ran
the soak, the short boots and another team's guest beside these runs.
That is no difference this measurement can show; a difference of the
size the arithmetic predicts (three or four requests of a few HMACs each,
against 600 ms per launch) is far below it.

**The soak** (section 4): 1 366 commands per hour against 1 246 in the first
hour of the previous soak and 1 314 over its whole length; `churn proc 8`
median 5 968 ms against 6 238 ms; 1 423 glibc launches in the hour against
1 356 per hour. Not slower.

**Boot.** `entropy::init` does the same work as before and only moved:
0.59 to 1.35 ms under KVM in the 104 short boots (median 0.78; the base:
0.58 to 1.51, median 0.75), 5.4 to 11.2 ms under TCG in 14 boots (the base: 4 to 9). Public image, R630,
`boot-time.py`, base (`b9a5755`) and final alternating
(`kvm-ab-boot-time.txt`):

| | base | final |
|---|---|---|
| round 3, 5 boots each, none of our other runs active: QEMU start to desktop | 2.44 to 2.76 s, **median 2.62 s** | 2.34 to 3.00 s, **median 2.55 s** |
| round 3: kernel entry to desktop (`[boot-time]`) | 794 to 839 ms, median 811 ms | 773 to 874 ms, median 812 ms |
| round 2, 10 boots each, the last short boots still running: kernel entry to desktop | 803 to 1 735 ms, median 901 ms | 794 to 1 728 ms, median 894 ms |
| round 1, 5 boots each, beside the soak and two lanes of short boots: kernel entry to desktop | 779 to 984 ms, median 867 ms | 844 to 1 584 ms, median 993 ms |

Rounds 1 and 2 are kept because they were run; they measure the host more
than the image (in round 2 both images have boots of 0.8 s and of 1.6 s).
Round 3 is the one to read: the same.

Under TCG the matrix boots reach the desktop in the same time as the base
(section 4); the new self-test itself is inside those numbers.

## 6. BUG-082, what it needs (not done)

The gate still rests on `estimate_jitter_bits`, a heuristic (a quarter bit per
sample whose difference from the one before has low four bits other than 0
and 15). It over-credits a near-periodic series; the memory-walk sample of
BUG-065 is not such a series, but nothing checks that at run time. Needed:

1. The two SP 800-90B health tests on the raw samples, in `crates/euroentropy`
   (host-tested): the repetition count test (cutoff `1 + ⌈20 / H⌉` for a false
   alarm rate of 2⁻²⁰) and the adaptive proportion test (window 512 for a
   non-binary source), with `H` the claimed min-entropy per sample. A batch
   that fails either is not credited and is counted; a source that keeps
   failing closes the pool for new seedings and says so in the `[entropy]`
   line.
2. A floor on variety: a batch drawn from a handful of values (the old
   `24, 24, 24, 28` series) is refused whatever the delta estimator says.
3. `H` from measurement, not from the quarter bit: raw dumps of the walk
   (a million samples each) from the R630, a NUC and TCG, through the 90B
   non-IID estimators (most common value, collision, Markov, compression,
   t-tuple, LRS) off line; the credit per sample becomes the smallest of
   them, halved.
4. The start-up test of 90B (1 024 consecutive samples through both health
   tests before the first credit) and the same tests on every later reseed
   (BUG-089).
5. Proof that every boot of every configuration still reaches 256 bits with
   the stricter gate: the 204-boot KVM loop and the jitter-only loops of
   `proof/integ-2026-11b` again, and how long `init` then takes.

## Open

- **A request before the early seeding, in one configuration.** With a blank
  virtio data disk attached (the soak guest: `--disks 8G`), the self-test image
  runs the install self-test `[q1x3]` in the storage phase, before the HPET:
  `instexec` → `euroid::install_db` → a salt. That request is now the first to
  ask and does the boot seeding itself: one status line, preceded by
  `[entropy] random bytes were asked for before the boot seeded the CSPRNG:
  seeding now` (`r630-soak-summary.txt`). The seed is the same (558 bits) and
  there is one pool, but `tpm::init` has not run at that point, so on a
  machine with a TPM this boot's seed would lack the TPM's bytes. Seen in the
  soak boot only; not in the matrix, the short boots, the q35 legs or the
  public image (the wizard installs from the desktop). Remedy: the two lines
  `tpm::init` / `entropy::init` move further up, before the storage phase;
  the duration in the status line then needs a clock that exists there (the
  HPET is brought up later). Not done: the images under test were built.
- **No TPM in any configuration** (no `swtpm` on the build server or the
  R630): the earlier `tpm::init`, a pool with TPM bytes, and the unchanged
  TPM arm of the `main.rs` seeds were not run. `tpm::init` uses spin counts,
  no timer, and reads the same MMIO window it read later in the boot.
- **Chromium.** The tracker asked for a chrome start with the fix. The one
  run made here (section 4) shows no difference to the base in 40 minutes of
  a chrome boot under TCG and no fault, but neither image got to a paint in
  that time with `chrome-pack2.img`. The Chromium team's own run on their
  current pack is still needed.
- **A read of a random device holds the fd-table lock for the whole request**
  (`vfs_read` calls `char_device_read` with `OPEN_FDS` held), as before; the
  generator is now an HMAC-DRBG, so a large read holds it longer. A cap per
  read (short reads are allowed above 256 bytes) or releasing the lock first
  would bound it. `O_NONBLOCK` on the devices is not looked at.
- **Yielding from a foreground program's syscall.** A first version of the
  wait in `getrandom` gave the CPU away between rounds
  (`if syscall_yield_ok() { yield_reacquire() }`, as `nanosleep` does). The
  musl run of the self-test, a foreground `run_args` launch with interrupts
  off, then died with exit 139 in its first blocking call.
  The Linux dispatcher sets the yield flag (`set_syscall_yield_ok(true)`) on
  its lock-free path, also for a program that is not a scheduled task. Not investigated further; the wait no longer
  yields. Any other yielding arm reached by such a launch would do the same.
- **`vfs_rename` over an existing file removes its entry from the file table**
  (`files.retain`), which moves every later entry down by one: file
  descriptors that are open on those entries, and the three device indices
  (`URANDOM_FI`, `URANDOM_ALIAS_FI`, `ZERO_FI`), then point at the neighbour.
  By reading, not provoked; it is older than this round and in the fd code
  the Chromium team is working in. With this round a random device that is
  no longer recognised reads as empty instead of as a fixed pattern. It needs
  a tracker number from the lead.
- **`[fuzz]` FAILED under KVM** in every boot, before and after (section 4).
- **BUG-082** (section 6), **BUG-086** (DHCP transaction id), **BUG-087** (SMB
  client challenge), **BUG-088** (width of the ASLR slides), **BUG-089** (no
  reseed after boot): in the tracker. BUG-087 is a two-line change that was
  left out only because the images were built.
- The file-system UUIDs are constants per call site (`EuroFs::format(…,
  [0x5A; 16], …)` for the root): every installation has the same one. Not a
  secret; noted because it turned up in the same search.
- `userland/randprobe.elf` is not in git (`userland/*.elf` is ignored; its
  signature is). The build server has no `musl-gcc`: the file was built on
  the R630 with `musl-gcc -static-pie -Os` from the committed source
  (sha256 `24765624e8f1…`) and copied into the worktree. A checkout that
  builds this branch needs that file in `userland/`, like the other musl
  programs; a copy is in `/opt/euroos/rand-runs/artefacts/`.

## Cleaned up

- Build server: the worktree's `target/` and the four driver `target/`
  directories, every image (`rand-runs/img`, `eurokernel.img`), the image
  copies of the harnesses and the symlink to `toolchain/eupkg/keys/dev.key`
  are removed. The serial logs and outputs stay in `/opt/euroos/rand-runs`.
- R630: the four images, the image and disk copies of the harnesses and the
  probe's source and binary are removed; logs and outputs stay in
  `/srv/euroos/rand-runs`. Nothing was installed. Every QEMU was started with
  `restrict=on` or without a NIC and stopped by its own PID.
- No git configuration was changed; the commits carry the identity in their
  environment.
