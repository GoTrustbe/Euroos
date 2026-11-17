# BUG-380 and BUG-371 (11f-bug380)

Branch `feature/11f-bug380` from `7b2eea6` (`feature/11f-aslr-s5`). Worktree `/opt/euroos/wt-11f-b380` on .16. Not pushed.
R630 run directory `/srv/euroos/b380-runs`, CPUs 16-19 and their siblings 36-39 only.

| commit | what |
|---|---|
| `a6b81a8` | tests first: `[vdso-mono]` strict (any step back, any order violation), part C (reads stalled between page loads and rdtsc), AP readers, cross-CPU check, diagnosis line, writer counters; `uptime` prints the vDSO clock |
| `f426198` | the fix: TSC read inside the seqlock (`lfence; rdtsc; lfence`) in `vdso.c` (rebuilt `vdso.so`) and in the kernel reader; `vdso_tick` makes the sequence odd before it reads the anchor TSC, interrupts off |
| (docs) | tracker BUG-380, BUG-371 fixed, BUG-400 new; this directory |

## Cause (proven)

`vdso.c` read the clock page in a seqlock loop and read the TSC **after** the loop:

    do { s1 = seq; base = [1]; tsc0 = [5]; rate = [6]; } while (seq != s1 || s1 odd);
    now = rdtsc();                     /* outside the seqlock */

A reader whose rdtsc came after the kernel had replaced the anchor (the tick landed between the check and the rdtsc,
or the host did not run that vCPU there) computed `old anchor + (now - old tsc0) * old rate`: the old rate run past
the new anchor. The new anchor starts at the old formula at the tick's TSC, but its rate is often lower (the BUG-355 slew
runs the clock at 7/8 or 1/4 while it is ahead of the tick count; a new calibration under TCG), so the next read is
lower than that stale one. The step is `(rdtsc - tick TSC) * (old rate - new rate)`: microseconds when the window is a
few tens of microseconds (TCG, a loaded host), up to milliseconds when a vCPU stops there for a whole tick. The kernel's
own copy (`vdso_clock`: clock_gettime, gettimeofday, the realtime futex) had the same order; that was the "system call
outside its two vDSO reads". The rdtsc was also not ordered at all (no lfence): BUG-371, same code.

Measured on the base (`a6b81a8` = `7b2eea6` + the test, `tests-first.txt`), the old formula and the ordered formula read
side by side in the same boot, same stalls:

| | KVM 1 / 2 / 4 vCPU, busy siblings | TCG 1 / 2 / 4 vCPU |
|---|---|---|
| old read (rdtsc after the check): steps back | 4 / 8 / 4 | 2 / 5 / 4 |
| ... of them right after a read that took its TSC after the anchor changed | **4 / 8 / 4 (all)** | **2 / 5 / 4 (all)** |
| ordered read (lfence; rdtsc; lfence inside the seqlock) | **0 / 0 / 0** | **0 / 0 / 0** |
| `[vdso-mono]` (reads as vdso.so) | FAILED (max 1.7 ms) | FAILED (max 1.7 ms; unstalled reads: boot 9.5 us, real 4.4-9.7 us in the first run) |
| AP readers: steps back / below another CPU's earlier read | - / 137 / 357 | - / 130 / 379 |

Ruled out:
- *Anchor and rate not read together / the anchor update jumps down*: the ordered read uses the same page and the same
  writer and never went back (0 in 12 boots), so the published anchors are continuous; every step of the old read came
  right after a read whose TSC was taken after the page changed. Writer counters: anchor TSC below the previous 0 times.
- *Concurrent or nested writers*: 0 writers entered while another was running, 0 on an AP, in every boot. (The task-context
  callers of `vdso_tick` can be interrupted by the tick in principle; the fix runs the update with interrupts off anyway.)
- *TSC frequency / calibration under TCG*: a changing calibration only makes the rate differ between anchors; with the
  ordered read it causes no step (TCG 0 in all legs). It is one of the two sources of the rate difference above.
- *Syscall and vDSO formula differ*: they were the same formula with the same flaw; both now read inside the seqlock.

## Fix

- `userland/glibc/vdso.c`: load seq (retry while odd), anchor, tsc0, rate; `lfence; rdtsc; lfence`; check seq again,
  read again if it changed. A TSC below tsc0 (another CPU slightly behind) adds nothing; a very old anchor gets the 2-tick
  cap instead of falling back to the anchor (that was a step back too). `vdso.so` rebuilt with the command now in the
  header of `vdso.c`.
- `vdso_tick` (`ring3.rs`): seq odd, `mfence; lfence`, then the anchor TSC: every reader that still gets the old page took
  its TSC before that store was visible, so before the new anchor's TSC, and the new anchor (max of the tick time and the
  old formula at that TSC) is never below what it saw. The whole update runs with interrupts off.
- `vdso_clock` / `vdso_interp` (kernel reader, system calls): the same read as `vdso.c`.
- The BUG-355 slew (anchor never below what readers saw, slower until the ticks caught up) is unchanged.

## vdso.so

`vdso.so` is a committed binary, built by hand (not by `userland/build.sh`), embedded in the kernel with `include_bytes!`
and copied into its own frames at boot. It has **no `.sig` and no hash** anywhere in the tree; it is covered by the kernel
image signature (`kernel.efi.sig`, made by the image build as before). Rebuilt on .16 with gcc 15.2 (`-Os`, `vdso.lds`,
`vdso.map`, `-Bsymbolic`, `--hash-style=both`, soname `linux-vdso.so.1`): one R+X PT_LOAD of 0x534 bytes (under a page),
no relocations, the same four `__vdso_*` symbols at version `LINUX_2.6`, one `lfence; rdtsc; lfence` and no other rdtsc.
`[vdso-mono]` checks the bytes of the embedded image and reads with that image's formula. No signature made, nothing
re-signed, `userland/build.sh` not run.

## Results (`f426198`)

| check | result | file |
|---|---|---|
| `[vdso-mono]` tests first, KVM 1/2/4 vCPU with busy siblings, TCG 1/2/4 vCPU | base: FAILED 6 of 6 (twice, 12 of 12) -> **OK 6 of 6**: 0 steps back (mono, boot, real), 0 system calls outside their vDSO reads, APs 0 steps back and 0 reads below another CPU's earlier read | `tests-first.txt` |
| TCG matrix, 10 legs (smep+smap, restrict=on), a loaded KVM lane beside it | **10/10 desktop**, 0 watchdog, 0 NMI sent, 0 crash; **`[vdso-mono]` OK in every leg** (smp4 legs: 3 APs, 670-695k reads, 1315-1358 stalled, 0 back) | `tcg-matrix.txt` |
| 30 KVM boots, busy loop on every sibling (10 x 1 vCPU, 10 x 2, 10 x 4) | **`[vdso-mono]` OK 30 of 30**, 0 crash lines | `kvm-30-loaded.txt` |
| KVM self-test boots 1/2/4 GiB x 1/2/4 vCPU, quiet, vs `7b2eea6` | 9/9 to the desktop, 0 crash lines; markers-compare **0 worse** in all 9 (0 changed: the base passes its old test on a quiet host) | `kvm-markers-compare.txt` |
| cargo test (default-members) | 188 suites, **1390 passed, 0 failed** (as S5) | `cargo-test-summary.txt` |
| gui-smoke, public image, KVM 4 vCPU 4 GiB | **67/67** | `gui-smoke.txt` |
| Chromium 20 starts, smp 2 | **20/20 PASS**, 0 aborts, 0 in the network service, 192/192 execs with a vDSO | `chrome.txt` |
| Chromium 6 site sessions, one round each | **6/6 PASS** (14/14 steps each), 0 aborts, 0 network service, 141/141 execs with a vDSO | `chrome.txt` |
| Chromium sites, 3 rounds in one session | **36/36**, 0 aborts, 43/43 execs with a vDSO | `chrome.txt` |
| consent banners | YouTube "reject all" clicked in every session that showed it (7 clicks); the other sites showed none | `chrome.txt` |

Ring-0 faults, panics, `[oom]`, FROZEN, children ended by a signal: 0 in every run above. Other FAILED lines are the known
ones of the base: `[aslr-ind]` (S6 not yet), GBIG malloc 200 MiB, and in the TCG legs the malicious-driver self-tests and
TCP to the outside under restrict=on.

## Drift

12 min per run, A and B at the same time on the same host (`drift.txt`, `drivers/vdrift.py`):

| run | before (A, `7b2eea6` clock) | after (B, fix) | tick count over the same samples |
|---|---|---|---|
| KVM 2 vCPU | -1145 ppm (-0.81 s) | -809 ppm (-0.57 s) | -1145 / -808 ppm |
| TCG 1 vCPU | -9705 ppm (-6.79 s) | -8671 ppm (-6.08 s) | -9700 / -8667 ppm |

The fix does not change the rate of the clock: in every run the vDSO clock follows `ticks x 10 ms` to within 5 ppm (the
BUG-355 design), largest offset from a straight line 19-68 ms. The tick count itself runs slow against the host, before
and after: filed as **BUG-400** (not fixed here; the difference between A and B is run-to-run noise of the tick loss,
both are within the same band).

## Deviations and open points

- Started from `7b2eea6` (`feature/11f-aslr-s5`) as the task said, not from `e4b8ba4` of the common rules.
- `a6b81a8` is an amended commit: the first version of the diagnosis attributed a step to the stale read itself instead
  of the read after it (it showed "0 of them"); `tests-first.txt` keeps those runs as `a-first`. Only the diagnosis text
  changed; the verdicts were FAILED in both.
- The exec'd-child half is still not measurable in the self-test (an embedded probe cannot exec); Chromium shows it: every
  exec'd child had a vDSO (376 of 376) and no child aborted.
- "Between processes" is covered by one shared clock frame (4 of 4 child calls map it) read from every CPU at once; a
  user-space probe in several processes would need a new signed binary (not made: no new `.sig`).
- Part C stalls a read for a fixed 1.5 periods plus 2 ms (BSP) or one period (APs); real stalls of other lengths are
  covered by the argument of the fix (the TSC is read inside the seqlock, any length), not by a sweep.
- The images were signed with the dev key in a temporary staging directory by the image build (as every earlier image);
  nothing signed was committed.
- Drift: BUG-400 found, not fixed. The KVM figure depends on host load (A and B ran beside a TCG pair).
- The chrome sessions ran with gui-smoke and the 20 starts beside them (8 CPUs for all lanes): host loaded.
