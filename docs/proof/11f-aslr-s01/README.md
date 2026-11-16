# ASLR steps S0 and S1 (docs/design/ASLR.md), `aslr=on` the default

Branch `feature/11f-aslr-s01` from `feature/11f-aslr-s23` `3a51bc6`. Worktree `/opt/euroos/wt-11f-aslr01` on .16. Not
pushed. R630 run directory `/srv/euroos/aslr01-runs`, CPUs 18-19 and siblings 38-39 only (two guests of 2 vCPU, or four
TCG guests of 1); no timing claims.

| commit | step |
|---|---|
| `6504c66` | S0: `euromm::layout` + host tests, boot option `aslr=on/off/legacy`, probe `gaslrx`, lines `[aslr]` (+vDSO), `[aslr-phys]`, `[aslr-ind]`, `[aslr-fork]`, `[aslr-exec]`, `[demand-slots]` |
| `deaba4d` | S1: demand zone 32..96 TiB over PML4 64..191, one draw per launch and per exec (exe base, mmap floor, vDSO), per-process layout in `ChildMem`, `[vdso-ro]` (tests first) |
| `0073feb` | BUG-350 (vDSO in fork children), BUG-353 (vDSO read-only) |
| `277b4dc` | `aslr=on` is the default |
| `1d95d9b` | BUG-355: no vDSO after an exec (as before), after the network service of Chromium aborted with one |
| (docs) | design section 8, BOOT-OPTIONS, tracker BUG-088 and BUG-350..355, this directory |

## Tests first (`tests-first.txt`)

| image | result |
|---|---|
| `6504c66` (S0, legacy) | `[demand-slots]` FAILED (slots 2..3 only: census 0 of 4 pages); `[aslr]` FAILED as before; every fork child of the probe died at the vDSO address (`rip=0x1f00000045e`) and its parent waited 60 s each (BUG-350, BUG-352) |
| `deaba4d` (S1), `aslr=on` | `[demand-slots]` OK; `[aslr]` mmap 1 MiB 30, libc 30, vDSO 27 bits; `[vdso-ro]` FAILED (image and clock page writable, BUG-353); `[aslr-fork]` FAILED (child died); `[aslr-phys]`, `[aslr-ind]`, `[aslr-exec]` FAILED as "not yet" |
| `deaba4d`, `aslr=off` | every region 0 varying bits (exe 32 TiB, floor 40 TiB, vDSO 100 TiB), `[aslr]` "off ⬜" |
| `1d95d9b` (final) | `[aslr-fork]` OK 8/8, `[vdso-ro]` OK, `[demand-slots]` OK |

## Entropy reached

| region | before | `aslr=on` | measured by |
|---|---|---|---|
| Chromium executable (from disk) | 0 | **30** | 20 boots: 20 distinct, 30 varying bits; 180 execs: 180 distinct, 30 bits (`layout-per-boot-and-exec.txt`) |
| mmap floor: libc, 1 MiB mmap, libraries, large reservations | 19 | **30** | `[aslr]` 64 starts at 1/2/4/8 GiB; Chromium 20 boots and 180 execs: 30 bits |
| vDSO (launched process; fork children share it) | 0 | **27** | `[aslr]`; Chromium 20 boots: 27 bits. None after exec (BUG-355) |
| embedded PIE, ld.so | 0 | 0 | arena still identity-mapped (S4, S5) |
| stack / brk / small mmap | 8 / 8 / 9-10 | 8 / 8 / 9-10 | arena slides (S5, S6) |
| distance stack-mmap, exe-stack, exe-mmap (`[aslr-ind]`) | 8-10 | 8-10 | one arena base (S6) |

`aslr=off`: 20 boots and 181 execs, one address each (exe `0x200000000000`, floor `0x280000000000`, vDSO
`0x640000000000`). `aslr=legacy`: exe `0x10000000000`, vDSO `0x1f000000000` in 20 of 20 boots, as `3a51bc6`.

## Results (`1d95d9b`)

| check | result | file |
|---|---|---|
| cargo test (default-members) | 188 suites, **1387 passed, 0 failed** (`3a51bc6`: 1381; +6 `euromm::layout`) | `cargo-test-summary.txt` |
| KVM self-test boots 1/2/4/8 GiB, 2 vCPU, HD-Audio | 4/4 to the end, 0 crash lines; markers-compare against `3a51bc6`: **0 worse** at all four sizes; new OK: `[aslr-fork]`, `[demand-slots]`, `[vdso-ro]`; new FAILED as "not yet": `[aslr-phys]` (exe, ld.so, stack, brk, mmap 4 KiB at their own physical address: S4), `[aslr-ind]` (S6), `[aslr-exec]` (ld.so kept: S5; vDSO none after exec: BUG-355); `[aslr]` FAILED as before (PIE 0, stack 8) | `kvm-markers-compare.txt` |
| gui-smoke, public image, KVM 4 vCPU 4 GiB | **67/67** | `gui-smoke.txt` |
| 30 TCG boots combined-smp1-iommu-1g | **30/30 desktop**, 0 watchdog, 0 NMI, 0 crash lines; `[aslr-fork]`, `[demand-slots]`, `[vdso-ro]`, `[arena-hi]`, `[elf-bias]` OK 30/30; 0 `[fuzz]`/`[churn*]` FAILED; markers-compare against a TCG boot of `3a51bc6` (same script): **0 worse in 30/30**; FAILED tags beyond base: only the three "not yet" lines | `tcg30.txt` |
| Chromium 20 starts, `aslr=on` | **20/20 PASS, 0 FROZEN**, 0 aborts | `chrome.txt` |
| sites, 3 rounds, `aslr=on` | **36/36**, consent banner of YouTube rejected by a click (round 1; later rounds have none), 0 aborts, 0 `[oom]`, 0 FROZEN, 0 ring-0 faults, 0 refused forks | `chrome.txt` |
| interaction, `aslr=on` | **18/18** | `chrome.txt` |
| local VP9+Opus video with sound (in the soak) | 2 of 2 rounds: picture 28/28, **12/12** five-second windows with sound, no media error | `soak/chrome-soak30.txt` |
| chrome-mem-soak 30 min, 8 GiB, `aslr=on` | 2 rounds, 35/41 steps; the 6 failures are the remote videos: YouTube "Sign in to confirm you're not a bot" (`soak/youtube-botcheck.png`) and Wikimedia "File not found" for the clip (`soak/wikimedia-file-not-found.png`), both rounds. 0 aborts, 0 FROZEN, 0 `[oom]`. demand pool free 2 564 -> 2 658 MiB between the rounds. mem-curve "NOT flat" (+317 MiB/h over the 15 min after its 15 min warm-up: too short for a slope; one sample of 25 MiB without an owner, the BUG-281 class) | `soak/chrome-soak30.txt` |
| Chromium 20 starts, `aslr=off` | **20/20 PASS**, 0 FROZEN | `chrome.txt` |
| sites, `aslr=off` | **36/36**, YouTube banner rejected by a click, 0 aborts, 0 FROZEN, 0 `[oom]` | `chrome.txt` |
| Chromium 20 starts, `aslr=legacy` | **20/20 PASS**, 0 FROZEN | `chrome.txt` |
| sites, `aslr=legacy` | **36/36**, YouTube banner rejected by a click, 0 aborts, 0 FROZEN, 0 `[oom]` | `chrome.txt` |

## Chromium and V8 reservations, memory (design risk 3)

`chrome-reservations.txt`. Per browser start (20 boots each) the reservations of 1 GiB and more:

| mode | 1 413 119 MiB (1.35 TiB, V8) | 528 383 MiB | 266 239 MiB | 32 / 16 / 4 GiB |
|---|---|---|---|---|
| `legacy` | 5 x **ENOMEM** | 5 x ENOMEM | 5 x granted | 15 / 10 / 10 |
| `on`, `off` | 5 x **granted** | not asked | not asked | 15 / 10 / 10 |

With the 64 TiB zone the 1.35 TiB reservation of every V8 process succeeds at the first try; with 480 GiB it failed
twice and V8 took a 260 GiB one. Address space reserved by all processes after a start: median 5 905 GiB (`on`)
against 1 717 GiB (`legacy`); it costs no frames. Memory actually used: process pool in use at the end of a start
median 429 MiB (`on`), 429 MiB (`off`), 444 MiB (`legacy`); page tables of the census 5 MiB in all three modes;
`unattributed` 0 KiB in every start. No other behaviour difference was seen.

## New bugs

BUG-350 (fixed), BUG-351 (open), BUG-352 (open), BUG-353 (fixed), BUG-354 (fixed), BUG-355 (open, avoided):
`docs/LOADTEST-TRACKER.md`. BUG-355 evidence: `bug355/abort.txt` (two aborts, symbolized stack).

## Not done

S4 and later (not asked). The vDSO after exec (BUG-355). What S4 needs: design section 8, last paragraph.
