# Fix of the D2 multicore regressions (feature/fix-d2, 2026-11-15)

Branch `feature/fix-d2` from `06e913e` (feature/validate-2026-11e, code of `48dfd05`), worktree `/opt/euroos/wt-fixd2` on .16,
merged with `feature/fix270` (`44da98a`, BUG-270, other team) as `f53eec1`. Not pushed. Reference for "good" is the release code
`3ecde9e` (public 2026.11.14), built here as public (`b0pub`) and browser image (`b0br`). R630 run directory `/srv/euroos/fixd2-runs`,
CPUs 4-19 and their SMT siblings 24-39; keylat and boot-time runs keep the siblings of a measuring guest idle and have their work
directories on tmpfs. `smpuser` stays off by default.

## 1. Commits

| Commit | Bug | What |
|---|---|---|
| `fe608dd` | BUG-271 | test first: `[kwork]` says FAILED when APs are online but their queues never opened (was "no APs ... (single core)") |
| `1830a79` | BUG-271, BUG-272 | an AP keeps its tick until its demo workers are parked |
| `4c8c363` | BUG-271 | a job queued behind a busy AP wakes an idle AP to steal it; the idle wait checks own and stealable work with IF=0 |
| `db9f0a6` | BUG-182 | test first: shell command `connif0` (`[conn-if0]`): tick gaps while a scheduled glibc connect waits |
| `5ea2dde` | BUG-182 | a network wait sleeps whenever its caller may, also with the in-kernel NIC; the receive interrupt wakes it |
| `48df3b0` | BUG-274 | while the desktop loop hosts an X client it is charged like that client (nice 0 instead of -10) |
| `93dbd07` | BUG-273 | a glibc program the desktop task runs in the foreground (`run_glibc`) gets the desktop task's weight |
| `f53eec1` | BUG-270 | merge of `feature/fix270` |

## 2. Cause and fix per bug

**BUG-271 / BUG-272.** S6 (`80c028d`) masks the periodic timer of an idle AP before it halts (`apuser::ap_idle_wait`). Before
`smp::init` parks the demo workers, only that tick (`sched::ap_schedule_tick`) switches to them, so they never ran (`worker-A=0`)
and never parked: `ap_park_demo_workers` waited its full 500 x 1 ms on every boot with APs and `kwork::start_accepting()` was never
called. Fix: the idle wait goes tickless only on a parked AP. With the queues open again, `[kwork]` at 4 vCPU showed a second
S6 effect: `work-stealing=NO`, because a tickless idle AP no longer looks for work to steal on its own. Fix: `kwork::submit` pings
one idle AP when the target core is inside a job; the idle wait checks for own or stealable work with interrupts off before it
halts. (`ap-kwork.txt`)

**BUG-274.** Bisected with browser images, paired runs (`keylat-bisect.txt`): `3ecde9e` 284-348, S0 `ab8e32c` 304-340,
**S1 `cdf5aff` 351-359**, S2 `80c0d69` 352-388, sched-d1 378-403 ms. S1 charges every turn by its length and gives the desktop loop
the weight of nice -10 (65 % against busy tasks, for BUG-192). With chrome hosted, the busy tasks are chrome's threads and the
loop's passes are work for chrome; at nine times chrome's weight the loop took the CPU while chrome waited: `sched-stat` over the
30 keys, desktop loop 7.2 s of 21 s at -10 against 3.0 s of 15.9 s at nice 0 (`keylat-schedstat.txt`). Desktop nice 0 or -5 restores
the keylat but fails `[fair]` (16 % and 34 % against 52 %); DESK_KICK variants did not help (vk 363-369, vq 380-413). Fix: the loop
reports once a pass whether a hosted X window exists; while it does, task 0 is charged at nice 0. Without an X client nothing
changes (`[fair]` 52 %). Not an S3-S6 step, so nothing was reverted.

**BUG-182.** `net::rx_should_block()` was true only for frames of the userspace driver. With the in-kernel virtio-net of the browser
images a blocking `connect` (also an ARP lookup, a receive) of a scheduled process spun with IF=0 for the whole bound of its wait:
1 s for the first SYN round when the SYN-ACK was late, which is the `[if0-window] 1009-1036 ms cause=syscall 42` of every FROZEN.
Reproduced with `connif0` and a QEMU `filter-buffer` that holds the answer back: 420 ms (SYN) and 948 ms (ARP) windows
`cause=syscall 42`, FAILED; fixed: 10 ms, 137-156 sleeps, OK (`conn-if0.txt`). Every network wait whose caller may sleep now sleeps a
tick after `RX_BURST` tries and polls the ring itself again; `rx_route_irq` wakes the waiters early with `try_unblock_any`.

**BUG-273.** Same lanes, same hour, short boots with every lane on both SMT siblings of a core busy (`fuzz-ab.txt`):
validate-2026-11d 0/32, `4c8c363` 17/32; S0 `ab8e32c` 0/32, **S1 `cdf5aff` 12/32**. Since S1 the fuzzer, started by the desktop task
which only waits for it, got one nice-0 share beside the ring-3 demo spinners of the self-test image instead of the CPU the waiting
task leaves, and ran into its 3 000-tick deadline. Fix: `run_glibc`/`run_glibc_disk` give the program `DESKTOP_NICE` when task 0
started it. The deadline and the 20 000 calls are unchanged.

**BUG-275.** Not investigated (time); rerun 20 times on the merged self-test image, TCG `combined-smp1-iommu-1g`, 20 at once: `[churn-forkthreads]` **OK 20/20**, desktop 20/20, 0 watchdog, 0 NMI, 0 crash lines; markers-compare against the validation rerun `rr3`: 0 worse in all 20 (`bug275-tcg20.txt`). Not reproduced, stays open as intermittent.

## 3. Before and after

| Measure | `3ecde9e` (release) | `48dfd05` (validation) | `feature/fix-d2` merged `f53eec1` |
|---|---|---|---|
| `[smp]` AP line, public image 4 vCPU | 3/3 idle in hlt | 0/3 | 3/3 |
| `[kwork]` self-test (2 and 4 vCPU) | OK | "no APs accepting work" | OK, work-stealing=yes at 4 vCPU |
| smp-timer phase, public, 2 vCPU | 44 ms | 551 ms | 54-60 ms |
| boot time median, public, 10 pairs alternating, pin 18-19 | 2.63 s (2.23-2.73) | 3.00 s (validation, against download 2.68 s) | **2.41 s** (2.21-2.79) |
| keylat smp 4, median ms, paired rounds | 284-326 (6 runs) | 395-413 (here, `4c8c363`) | **200-236 (5 runs)** |
| keylat smp 2, median ms, paired rounds | 304-347 (6 runs) | 398-416 (validation) | **214-274 (6 runs)** |
| `[conn-if0]` longest window without a tick | - | 420 ms (SYN), 948 ms (ARP) FAILED | 10 ms OK |
| browser boots without FROZEN | ~30/30 (validation) | 52/55 (validation) | **60/60 starts + 15 keylat boots, 0 FROZEN** |
| `[fuzz]` FAILED, 16-lane sibling-sharing load | (11d: 0/32) | 17/32 (`4c8c363`) | **0/112** |
| `[fair]` hogtest 5 + churn 8, lowest task 0 share | - | 52 % | 52 % OK (quiet); see note |

## 4. Final regression of the merged branch (`f53eec1`)

| Test | Result |
|---|---|
| `cargo test` | 188 binaries, **1 381 passed, 0 failed**, 1 ignored (as validation) |
| KVM self-test boots 1/2/4/8 GiB, 2 vCPU, HDA (`final-kvm.txt`) | desktop 4/4, 0 crash lines; FAILED only `[aslr]` (BUG-088, known) and the two `GBIG` at 1 GiB; markers-compare **0 worse** against `48dfd05` and against validate-2026-11d at all four sizes; only new: `kwork: OK` |
| gui-smoke, public image, 4 vCPU 4 GiB | **PASS 67/67** |
| boot time median (above) | **2.41 s** (limit 2.8 s) |
| 100 short boots, self-test image, 16 lanes on both SMT siblings of 8 cores | **112 boots, 0 FAILED lines, 0 crash lines**, CSPRNG seeded 112, `[3d8]`/`[3d7]` OK 112; `[fuzz]` after 33.7 s median, 39.2 s max (`short-boots.txt`) |
| keylat quiet, smp 4 and smp 2, paired against `3ecde9e` (`final-keylat.txt`) | smp 4: `f53eec1` **200 223 232 / 236 215** against `3ecde9e` 323 284 325 / 307 326 305 ms; smp 2: **214 265 243 / 274 247 234** against 310 314 347 / 342 304 345 ms: faster in every pair (`final-keylat.txt`; one smp 4 run did not reach the key step, harness omnibox check, seen before on d2-reg images) |
| Chromium 20 starts | **20/20** (merged image), plus 40/40 on `48df3b0`; 0 FROZEN (`chrome-starts.txt`) |
| BUG-275 rerun, TCG `combined-smp1-iommu-1g`, 20 times | `[churn-forkthreads]` **OK 20/20**, 0 worse (`bug275-tcg20.txt`) |

## 5. Not done / notes

- The 2-hour soaks (validation team).
- The smp-timer phase is 54-60 ms against 44 ms on `3ecde9e` (2 vCPU); boot time is lower anyway.
- BUG-182: only the network waits were changed; the sched-d1 `[if0-window]` attribution stays and would name any other IF=0 window.
- BUG-274: the desktop loop's own cost per pass while chrome draws (passes of 100 ms and more in `[desk]`) is unchanged; worth a look.
- `[fair]` (hogtest 5 + churn 8, lowest of 10 rounds >= 50 %): merged image 52 % OK on the quiet machine; 49 % once under the TCG and
  starts load, and `4c8c363` (before BUG-273/274) 48 % in the same quiet run as the 52 %: the first round dips, as the S1 commit
  already reported (lowest 47 %). Not caused by these fixes (desktop share per round unchanged); not given a bug number here.
  `connif0` on the merged image: 10 ms, OK (`final-fair-connif0.txt`).
- `scripts/chrome-smoke-ss.py` (sched-stat around the keys) was a local R630 copy for the measurement, not committed.
