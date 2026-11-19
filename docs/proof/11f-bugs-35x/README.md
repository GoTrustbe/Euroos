# BUG-351, BUG-352, BUG-355 (11f-bugs-35x)

Branch `feature/11f-bugs-35x` from `c71cce8` (`feature/11f-aslr-s01`). Worktree `/opt/euroos/wt-11f-b35x` on .16.
Not pushed. R630 run directory `/srv/euroos/b35x-runs`, CPUs 16-19 and siblings 36-39 only; no timing claims.

| commit | what |
|---|---|
| `d839c89` | BUG-351 (first rule) and the three self-tests `[fork-cow-write]`, `[fork-fault-wait]`, `[vdso-mono]` (`kernel/src/forkvdsotest.rs`) |
| `b2c78a0` | BUG-352: a fork child ended by a fault in ring 3 is reported to wait4 |
| `eed2a95` | BUG-355: the vDSO clock never steps back; exec'd children get a vDSO again (code of `1d95d9b` reverted) |
| `b4355d7` | BUG-351: a read-only mprotect of a file mapping keeps its EFAULT (Chromium ProtectedMemory) |
| `537de96` | `[vdso-mono]`: steps back under 1 us reported apart (BUG-371) |
| (docs) | tracker BUG-351/352/355, BUG-370..372, this directory |

## Cause and fix

| bug | cause | fix |
|---|---|---|
| 351 | not copy-on-write: `demand_file_backed` refused any user range overlapping any file mapping; libc's flat whole-library mapping lies under its .data and .bss, so getrandom, clock_gettime, socketpair, ... into them gave EFAULT (pipe2 returned 0 without storing its descriptors) | the newest mapping of each page decides (.bss overlay = anonymous); a file page the process holds writable or shares copy-on-write takes the write unless mprotect made it read-only (`RO_FILE_RANGES`, per address space); pipe2 gives EFAULT |
| 352 | `fault_kill_group` handled the launched process only: a faulting fork child lost one task, nothing reached wait4 | the whole fork child ends and is torn down from another process's context with status "killed by signal" (SIGSEGV/SIGILL); no SIGCHLD because this kernel delivers no signals at all (normal exits neither) |
| 355 | the vDSO clock stepped back up to 10 ms: anchor `ticks * 10 ms` + up to 20 ms of rdtsc interpolation, and a late tick counts as one period; the system call (bare ticks) lagged up to 20 ms | anchor = max(tick time, what readers see now), slewed back to the tick count (1/8, 3/4 from 5 ms); REALTIME derived from it; clock_gettime/gettimeofday/futex realtime read the same page; vDSO after exec again at the new draw |

## Tests first (`tests-first.txt`)

| test | c71cce8 + test | `537de96` (KVM 1/2/4 GiB 2 vCPU, 2 GiB 4 vCPU) |
|---|---|---|
| `[fork-cow-write]` | 1 of 26 writes ok (EFAULT, "parent's bytes changed" was a test error, fixed in t3) -> FAILED | 26 of 26 -> OK |
| `[fork-fault-wait]` | wait4 gave up after 60 s, no status -> FAILED | status 0xb (SIGSEGV), run 0 s -> OK |
| `[vdso-mono]` | steps back 9998 us (51-154 per run), system call outside its two vDSO reads 50-63 M times (max 20 ms) -> FAILED | 0 steps back over 1 us, 0 order violations -> OK (sub-us steps: 2 of 4 boots, max 401 ns: BUG-371) |

The first BUG-351 rule (`d839c89`: any written file page takes the write) passed the self-test but made every Chromium
renderer abort in `base::internal::CheckMemoryReadOnly` (ProtectedMemory expects EFAULT on its read-only page):
161-205 aborts in each of 3 sessions (`bug351-renderer-abort.txt`). `b4355d7` records read-only mprotects; 0 aborts since.

## Results (`537de96`)

| check | result | file |
|---|---|---|
| cargo test (default-members) | 188 suites, **1387 passed, 0 failed** (as `c71cce8`) | `cargo-test-summary.txt` |
| KVM self-test boots 1/2/4 GiB, 2 vCPU, HD-Audio | 3/3 to the end, 0 crash lines; markers-compare against `c71cce8`: **0 worse** at all three; new OK: `[fork-cow-write]`, `[fork-fault-wait]`, `[vdso-mono]`; also 2 GiB with 4 vCPU: all three OK | `kvm-markers-compare.txt` |
| gui-smoke, public image, KVM 4 vCPU 4 GiB | **67/67** | `gui-smoke.txt` |
| Chromium site sessions, one round each (start + 6 sites + YouTube consent), smp 2, 6 GiB | **16/16 PASS** (14/14 steps each), **0 aborts, 0 in the network service**, 0 crash/oom/frozen lines; every exec'd child has a vDSO (371 of 371 exec lines, each at its own address) | `chrome.txt` |
| Chromium 20 starts | **20/20 PASS**, 0 aborts, 195/195 execs with a vDSO | `chrome.txt` |
| sites, 3 rounds in one session | **36/36**, 0 aborts | `chrome.txt` |
| consent banners | YouTube "reject all" clicked in all 17 site sessions (r2/r3 of the long session had none left); the other sites show none | `chrome.txt` |

## Not done / open

- BUG-370 (fstat of a pipe end: EBADF), BUG-371 (vDSO rdtsc without lfence: sub-us steps back; needs a rebuilt
  `vdso.so`), BUG-372 (kernel write into an untouched page of a writable file mapping still EFAULT): tracker.
- No SIGCHLD (no signal delivery in this kernel). A fork child's task that overflows its kernel stack (`[g1]`) is
  still ended alone.
- The exec'd-child half of `[vdso-mono]` cannot run in the self-test (an embedded probe cannot exec); Chromium's exec'd
  children show it (`[aslr-layout] exec ... vdso`, every exec has a vDSO at its own address).
