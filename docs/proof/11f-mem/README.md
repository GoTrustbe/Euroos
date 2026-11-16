# 11f-mem: memory bugs BUG-243, BUG-281, BUG-230, BUG-157

Branch `feature/11f-mem` from `e4b8ba4` (worktree `/opt/euroos/wt-11f-b`, not pushed). R630 CPUs 12-17 and 32-37,
run directory `/srv/euroos/11fb-runs` (removed at the end). Tracker: BUG-157, 230, 243, 281 updated, BUG-310 new.

| commit | what |
|---|---|
| `1913094` | the tests first: `[eurofuzz]` times every call (limit 50 ms; `[slowcall]` names calls of 20 ms or more); `[memt] cowfork` takes a census in the middle of a fork |
| `7c8fc5c` | BUG-230: a Euro `log()` line goes to the serial port bounded (160 characters); the background path records its last call |
| `5861686` | BUG-281: the census walks a fork in progress and is taken again when the process list moved |
| `949be13` | BUG-243: the comment at the holder counts names the cause found (no code change) |
| `1a6edd9` | BUG-281: the range lists of a fork in progress are read with interrupts off |

## Per bug

**BUG-243** (`bug243.txt`): cause found, no memory bug, workaround kept.
Reproduced with the holder counts back in frames on `3f6bdb3`: TCG 1 GiB storage and combined, `[bgnet-cap]` FAILED
2 of 2, msock at 0xc200000, task 22, PML4 0x206000 (as reported). Debug build: msock Ready, 0 system calls, 0 page
faults, resume frame still at its entry (rip 0xc201261), code pages equal a fresh load. Its vruntime was 112-345 ticks
of nice 0 ahead of five busy self-test tasks: the scheduler before S1 charged a step per tick, a new task got the
vruntime of its parent (the desktop task, which takes every other tick and so runs about five times faster than the
pack), and the spinners did not catch up within the 30 s of the test. The placement only moved the timing. With S1
the same placement passes: `b6eb05d` 2/2, `e4b8ba4` 2/2 (msock again at 0xc200000, PML4 0x206000). The counts stay in
the kernel heap (4 MiB at 8 GiB; boot layout unchanged for the release). Scheduler not touched (team A).

**BUG-281** (`bug281.txt`): accounting gap, fixed. Every "without an owner" peak of the 2 h sessions is one sample
(76 MiB once in 324; 94 and 57 MiB in another 324; all others -3..+1 MiB) at a census that met a browser fork: the
parent's pages were already shared copy-on-write (chrome anon 77 -> 41 MiB in that sample), the child not yet listed.
Test `[memt] cowfork`: 30 852 KiB without an owner in the middle of the fork -> 0 KiB (KVM 1/2/4/8/16 GiB).
1 h at 8 GiB on the final image: verdict **flat**, in use +10.5 MiB/h (0.3 % of the pool), 3 MiB without an owner at
the worst (was 40-94 MiB), 0 kills. The slope was always under the 5 % limit, so `3ecde9e` was not run.

**BUG-230** (`bug230.txt`): fixed. Not madvise: `[eurofuzz]` calls Euro `log()` with 4 KiB of binary data, which went
to the UART with interrupts off (79-163 ms); the "syscall 28" was a stale LAST_SYS entry of task slot 30.
`[eurofuzz]` longest call 112 ms FAILED -> 6-10 ms OK; 16 GiB 4 vCPU: windows of this cause 11 (81-148 ms) -> 0.
Left: 50-75 ms read/write/rt_sigprocmask windows and ~250 ms `cause=kernel` of task 0, there before (BUG-310).

**BUG-157** (`bug157.txt`): closed, cannot reproduce. Two s4 sessions (4 GiB, 2 vCPU, site matrix without video,
2 x 15 min): 0 hangs, 0 NMI probes (one OSM "still changing" step). No hang in any other session of this round either
(40 Chromium starts, 2 x 3 site rounds, two 1 h sessions at 8 GiB, two gui-smoke runs).

## Final check (images of `1a6edd9`)

| check | result |
|---|---|
| cargo test (default members) | 1381 passed, 0 failed, 1 ignored (run on `949be13`; `1a6edd9` changes kernel code only) |
| KVM self-test boots 1/2/4/8 GiB, 2 vCPU, markers-compare vs `e4b8ba4` (`markers-compare-kvm.txt`) | **0 worse** at every size; `[eurofuzz]` 7-10 ms, `[memt] cowfork` census 0 KiB |
| gui-smoke, public image, KVM 4 vCPU 4 GiB (`chrome-gui.txt`) | **67/67** |
| Chromium 20 starts, smp 2 | **20/20** |
| sites, 3 rounds, smp 2, banners clicked | **36/36** |
| chrome-mem-soak 1 h, 8 GiB, local VP9+Opus (`chrome-soak1h.txt`) | 6 rounds, **99/99 steps**, verdict **flat**, 0 kills, VP9 playing in every round |

Earlier image (`5861686`, before `1a6edd9`): starts 20/20, gui-smoke 67/67, 1 h soak flat (4 MiB without an owner),
sites **34/36**: YouTube once with the desktop loop answering after 1 502 ms (limit 1 000; page rendered,
`screenshots/5861686-sites-youtube-1502ms.png`) and OSM "still changing". Reading that code again found the lock order
fixed in `1a6edd9`; that it caused the 1.5 s is not proven.
