# Kernel bugs BUG-111, BUG-088, BUG-089, BUG-082 (2026-11-15)

Branch `feature/kbugs-2026-11e` from `3ecde9e` (worktree `/opt/euroos/wt-kbugs` on .16). Not pushed.
Every bug test first: a test that fails on the commit before the fix. Runs on the R630 (KVM, CPUs 8-23),
run directory `/srv/euroos/kbugs-runs`; serial logs and images stay there. Scripts used: `drivers/`.

| Commit | What |
|---|---|
| `340f185` | BUG-111 test: boot self-test `[rename-disk]`; the Terminal write-back as one function (no change) — **FAILED** |
| `1361dc2` | BUG-111 fix (`kernel/src/fsback.rs`) |
| `a49e6a1` | BUG-089 test: `scripts/restore-reseed.py`, shell command `random` — **FAIL** (equal bytes) |
| `2e5807c` | BUG-082 health tests (`crates/euroentropy`, `tests/bug082.rs` fails on the old crate) and BUG-089 reseed |
| `aa1a096` | BUG-088 test: boot self-test `[aslr]`, probe `userland/glibc/gaslr` |
| `8c3b146` | BUG-088, partly: every slide in 4 KiB steps |
| `ad38d73` | `gaslr.sig` (run_glibc refuses unsigned programs; `[aslr]` of `aa1a096` was refused) |

Images of `ad38d73` (`scripts/build.sh`, `-j 6`, `nice 10`): self-test `81004bd9395ecfc5`, public `060df18b4da6737a`.
Before-image of BUG-088: `ad38d73` with the ring3 hunk of `8c3b146` reverted, `d03a06c8d58d944d`.

## 1. Per bug

**BUG-111 — fixed.** Cause: the Linux ABI works on a file table in RAM; after a program the Terminal wrote back
only paths opened for writing that still had that name. An atomic save (`a.txt.tmp` renamed over `a.txt`) left the
dirty mark on the temporary name, `unlink` never reached EuroFS, and a file of an earlier boot was not in the next
boot's table. Fix: `rename` carries the dirty mark; while the Terminal runs a program it lends it its file system
(`ring3::with_backing_fs`): an unknown path is looked up in EuroFS on first use (open, stat, `O_CREAT`, unlink, old name
of a rename); unlink and old rename names are recorded and `ring3::write_back` removes them from EuroFS (unless the
name is written again), then writes the dirty files. Outside a Terminal run nothing changes.

| | before | after |
|---|---|---|
| `rename-persist.py` (install, 3 boots, power cut) | 21/24 in 44 runs (validate-2026-11d) | **24/24 in 3 of 3 runs** (`a49e6a1`), 24/24 on the final image |
| `[rename-disk]` | FAILED: save 2 not on EuroFS, check 2 fails (`rename-disk-before.txt`) | OK at 1, 2, 4, 8 GiB |

**BUG-089 — fixed.** Cause: the HMAC-DRBG state lives in RAM and was only reseeded at boot. Fix: the ACPI VM
generation ID (`VGIA` in an SSDT, ID at +40) is read before every request; a change reseeds from fresh noise (RDSEED,
RDRAND, one jitter batch, health-tested) plus the ID, TSC and HPET, before a byte is served. Without an ID the desktop
loop compares RTC and HPET once a second (a restore moves the RTC, not the HPET; > 10 s marks a reseed). Plus a reseed
every 5 minutes.

| `restore-reseed.py`: savevm, two new QEMUs from the snapshot, `random` in each | result |
|---|---|
| before (`a49e6a1`) | both restores `513fdc3952ec1c66…` — **identical** (3/4 checks) |
| after, `-device vmgenid,guid=auto` | `5c05ffe9…` vs `351c964a…`, `[entropy] reseeded: the VM generation ID changed` (4/4) |
| after, no vmgenid | `8fee5920…` vs `1797ff8e…`, `reseeded: the wall clock jumped against the HPET` (4/4) |
| final image | 4/4 |

**BUG-082 — fixed.** SP 800-90B repetition count + adaptive proportion tests on the raw samples. Jitter: cutoffs for
H = 1 bit/sample (RCT 21, APT 311/512; the host test recomputes 311 from the binomial); a failing batch is discarded,
the next one tested afresh. RDSEED/RDRAND: two equal words in a row switch the source off; TPM bytes: RCT with H = 4.
On failure: `[entropy] health test FAILED on <source>: … → batch discarded / source disabled`, no panic.
`tests/bug082.rs` (periodic series must not open the pool; stuck RDRAND words not used): 2 FAILED on the crate of
`3ecde9e`, 2 ok after (`bug082-host-tests.txt`). 11 model tests: measured walk and a 2-bit source 0 failures in
20 000 batches each, 1-bit source at the cutoff ≤ 20 (expected ~5), 0.5-bit source caught in 713/1 000. Boot self-test
`[entropy-health]` OK in every boot; the real boot seeding had 0 health failures in every KVM and TCG boot of this round.

**BUG-088 — partly, stays open.** `[aslr]`: 64 starts of a glibc PIE, bits that differ between starts:

| region | before | after | Linux (.16, same probe) | target |
|---|---|---|---|---|
| PIE base / ld.so | 0 / 0 | 0 / 0 | 34 / 32 | 28 |
| mmap window (4 KiB map) | 5 | 9 to 16 (by RAM size) | 32 | 28 |
| demand pool (1 MiB map, libc) | 11 | **20** | 32 | 28 |
| stack | 8 | 8 | 30 | 22 |
| brk | 4 | 8 | 34 | — |

Why not the Linux level: the glibc arena (exe, ld.so, brk, mmap window, stack) is one identity-mapped span
(virtual = physical), so the PIE base cannot move and the in-arena slides are bounded by the arena size; the demand
pool is one PML4 slot (480 GiB, chrome reserves > 256 GiB). Needed: a virtual arena at a random base (fork children
already run virt ≠ phys via `build_address_space_remap`; ~30 identity-map accesses in `ring3.rs` to convert) and a
demand pool over several PML4 slots. Not attempted in this round: too invasive to do safely next to the rest.
`[aslr]` therefore prints FAILED in every self-test boot (a new tag, not a regression).

## 2. Regression (final images, `ad38d73`)

| Test | Result | Comparison |
|---|---|---|
| `cargo test` on .16 (`cargo-test.txt`) | 188 binaries, **1 375 passed, 0 failed**, 1 ignored | validate-2026-11d: 187 / 1 363 (new: `tests/bug082.rs`, 11 health tests) |
| KVM self-test boots 1, 2, 4, 8 GiB, 2 vCPU, HD-Audio (`kvm-selftest-boots.txt`) | desktop in all four, 0 crash lines; `[rename-disk]`, `[entropy-health]`, `[reseed]`, `[rename]` OK in all; FAILED lines: `[aslr]` (BUG-088, open) and at 1 GiB the known `GBIG malloc(200 MiB)` | markers-compare against validate-2026-11d: **0 worse at all four** (`comparison-kvm.txt`); new: aslr FAIL, entropy-health/rename-disk/reseed OK. Boot 279/325/351/347 s (base 4 GiB 285 s): `[aslr]` costs about 50 s |
| TCG matrix of 10, restrict=on (`tcg-summary.txt`) | **10/10 desktops**, 0 watchdog, 0 NMI, 0 crash, 0 FROZEN, 0 health failures; `[aslr]` SKIP at 512 MiB (no arena), measured at 1 GiB | **0 worse in 10/10** against validate-2026-11d (`comparison-tcg.txt`); boot times 210 to 832 s (base 220 to 757) |
| `gui-smoke.py`, public image, KVM 4 GiB (`gui-smoke-public.txt`) | **PASS 67/67** | markers against validate-2026-11d public: **0 worse, 0 changed** (`comparison-public.txt`) |
| `rename-persist.py`, public image (`rename-persist-final-ad38d73.txt`) | **24/24** | base 21/24 |
| `restore-reseed.py`, public image (`restore-reseed-final.txt`) | **4/4** | base 3/4 |

## 3. Not done, deviations

- BUG-088 not at the Linux level (section 1); `[aslr]` FAILED in every self-test boot until it is.
- New: **BUG-210** (a directory listing by a Linux program does not show files that are only on EuroFS; found by
  reading, not measured) and **BUG-211** (the first write-back after a self-test boot writes the self-tests' files to
  EuroFS, two named `\u{1}` and `\u{4}`; since this round that write-back is `[rename-disk]` in the boot).
- BUG-082: no off-line SP 800-90B min-entropy assessment on raw dumps.
- BUG-089: without a generation ID the first request after a restore can precede the clock check (≤ ~1 s); without an
  HPET only the 5-minute reseed remains. Flush on `close`/`fsync` (BUG-111) was not added: the write-back stays at the
  end of a Terminal program, as the bug asked; the browser keeps its files in RAM as before.
- `gaslr.sig` is a new signature for a new program, made with the dev key (as `grenprobe.sig`); no existing `.sig` re-signed.
- Not run: q35 metal matrix, soak, Chromium runs (not asked).
