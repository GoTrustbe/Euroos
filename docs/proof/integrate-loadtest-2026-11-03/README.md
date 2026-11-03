# The load-test fixes merged into app-control: boot proof (2026-11-03)

Branch `feature/integrate-loadtest`: `feature/app-control` at `e2b4c96`
(`bd7d0e0` plus a `.gitignore` line from the nightly sync: the four
userspace drivers, one vector pool, one driver-syscall dispatch) with
`feature/loadtest-r630` (`36bcd7b`: BUG-012 to 018, 022, 024, 025, BUG-023
bounded, the LOADTEST-R630 report and proof) merged. What the merge commit
resolved and added is in its message; in short:

- Two conflicts. `TcpConn::poll_seg` keeps app-control's `RxWait` (a wait
  that may give the CPU away) with the load test's three-argument
  `portq_pop` (BUG-018). `reap_dead` closes a dead process's sockets first
  (app-control), then gives its scheduler slots back and drops its futex
  waits (BUG-014/017).
- `rx_route_irq` (the NIC interrupt drains the ring, app-control) sends a
  bare SYN to the legacy queue, as `rx_route` does since BUG-018. Without it
  a SYN drained from the interrupt would never reach listen/accept or httpd.
- With BUG-014 a task slot is reused. `reap_dead` now clears the per-task
  policy state of a dead task before its slot is reclaimed (capabilities,
  sandbox profile and filter bit, syscall-yield and network-block flags) and
  revokes the udrv grants of the reaped pid (`udrv::revoke_pid`): a grant is
  looked up by task slot, and the supervisor may notice the death after the
  slot already belongs to someone else. In the R630 logs below the
  restarted e1000d and the native apps get the slots of reaped processes
  (task 27, tasks 20 to 22) with their own capabilities and profiles.
- `spawn_bg_app` (native apps and every userspace driver) refuses a start
  when the task table is full or no kernel stack is free, as BUG-015 made
  it for musl processes, fork and clone; the udrv supervisor retries.

Full serial logs are kept next to this file on the build server (`*.log` is
not committed); the `.markers` files hold the driver, SMP and lock lines of
those logs. `scripts/markers-compare.py` made the comparisons in
`comparison.txt` and `r630-comparison.txt`.

## Build server (QEMU 10.2, TCG)

The same runs as `../integrate-drivers-2026-11-02/`: `-machine q35 -cpu
qemu64,+smep,+smap`, selftest image (`scripts/build.sh release`), `-nic
user,model=e1000e`, each boot until 120 s after `[desktop] interactive loop
started`. Two QEMUs at a time (`nice`), each killed by its own PID.

| Log | Run | Result vs `../integrate-drivers-2026-11-02/` |
|---|---|---|
| `user-smp4-iommu`, `user-smp1-iommu` | defaults (`net.driver=user`), 512 MiB, IOMMU | identical per-marker verdicts |
| `user-smp4-noiommu`, `user-smp1-noiommu` | the same without IOMMU | identical |
| `storage-smp4-iommu-1g`, `storage-smp1-iommu-1g` | `storage.nvme=user`, 1 GiB, IOMMU, blank 64 MiB NVMe disk | identical |
| `combined-smp4-iommu-1g` (+ `-r2` … `-r6`), `combined-smp1-iommu-1g` | as storage, plus `megasas-gen2` with two logical drives | identical except `[sbx2]` (below) |
| `mr-iommu-boot{1,2}`, `mr-noiommu-boot{1,2}` | `scripts/megaraid-proof.sh` with and without IOMMU | identical; host-side checks identical |

Every run reached the desktop. `[lk] boot-total order inversions=0
task-lock-with-IF0=0` in every run. The only `FAILED` lines are the base's:
at 512 MiB the glibc tests without an arena, at 1 GiB the glibc set that
already fails there.

`[sbx2]` at 1 GiB: its rlimit, filter or ASLR sub-test fails when glibc
cannot map `libc.so.6` ("failed to map segment from shared object") from the
1 GiB demand pool. It passed in the base's `combined-smp4` log and failed in
5 of 6 runs here. Three base boots (`e2b4c96`, built the same way), each run
at the same time as one boot of this tree: base 1 of 3 OK, this tree 0 of 3
OK; the two pairs where both failed are identical marker for marker. Base
over all four: 2 of 4 OK; `combined-smp1` fails it on both trees, as does the
base's `storage-smp1`. Same cause on both trees; this tree's demand pool is
2 MiB larger (142 vs 140 MiB), not smaller. Not counted as a regression of
the merge; the 1 GiB glibc flakiness stays open.

Host tests of the whole workspace (`cargo test`, default members): green.

## Dell R630 (KVM)

The merged tree carried there as a git bundle and built on the host with
its throwaway dev key (the pre-signed artifact tests fail by construction
there, see `../loadtest-r630-2026-11/`). `-machine q35,accel=kvm -cpu
host,host-phys-bits-limit=39`, `-nic user,model=e1000e,restrict=on` (no route
out: the network markers that need the internet fail on both trees), no
VFIO device. Base `e2b4c96` built and booted the same way for comparison.

| Log | Run | Result |
|---|---|---|
| `r630-kvm-smp8-4g` | `-smp 8 -m 4G` | desktop after 52 s; 8 cores, `[kwork]` OK; processes with arenas above 1 GiB run; e1000d up, crashed and restarted |
| `r630-kvm-smp16-8g` | `-smp 16 -m 8G` | the same; `[smp] MADT lists 16 enabled core(s); EuroOS runs at most 8 … using 8, 8 left offline` (BUG-012) |
| `r630-base-e2b4c96-kvm-*` | base, same two runs | every background process dies at its first instruction (BUG-016), e1000d included: `[udrv-e1000]` and `[net-late]` FAILED |

Compared with the base (`r630-comparison.txt`): no marker worse; better
`[euroapp]`, `[eurofuzz]`, `[udrv-e1000]`, `[udrv-vec]`, and the network
part of the boot runs at all.

```
[euro] bg-musl (pid 8) -> task 10, own address space PML4 0x150000, arena 0x42200000
[udrv] e1000d started: pid 131073, task 27, profile driver · …
[kwork] per-CPU work queues: 7 AP(s) online, 8 worker(s) ran alloc+format+SSE jobs with a correct per-CPU id (8 ok), work-stealing=yes -> OK ✓
[lk] boot-total order inversions=0 (self-test provoked=2) task-lock-with-IF0=0
```

Base, same machine and options:

```
[isolation] ring-3 page fault addr=0x4f001fad code=PageFaultErrorCode(PROTECTION_VIOLATION | USER_MODE | INSTRUCTION_FETCH) -> process pid 131072 (task 21) TERMINATED
[udrv-e1000] e1000d pid 131072 did not report link up within 10 s → FAILED
```

Inbound TCP with the userspace NIC driver (the default now),
`scripts/loadtest-accept.py IMAGE DIR 6` (`r630-accept-serial.log`):

```
[bootopt] no euroos-boot.conf (defaults): net.driver=user, storage.nvme=kernel
round 0: guest accepted=True client=ok latency=0.26s shell=1262ms
…
[accept] 6/6 inbound connections accepted and answered
```

`net` scenario with 4 host clients on the background httpd
(`r630-net-hammer.txt`): 79 commands OK (baseline checks and outbound
fetches), then the console went
silent for more than 60 s with `[3g2-wire] WATCHDOG TRIPPED`; httpd 39
answered, 50 failed, p99 7.4 s. That is BUG-019 as the tracker describes it
(synchronous inbound handling starves the loop; one harness HANG in the
load-test runs too); no panic.

Soak, 1 h (`scripts/loadtest.py --kvm --smp 4 --mem 16G --disks 8G --nic
e1000e --host-http --scenario soak --duration 3600 --procs 32`, glibc tests
re-signed locally with the throwaway key as in the load test):
`r630-soak.csv`, `r630-soak-ltstat.csv`.

| | |
|---|---|
| commands | 2 085, all ok; `completed clean` |
| tasks / files / kernel stacks / process pool | 55 / 186 / 0 / 887 296, flat from first to last sample |
| free frames | 2 964 998 at the first sample (32 zombies not yet reaped) → 2 981 638 at the last |
| kernel heap | 147.9 → 149.8 MB (+31 kB/min, BUG-027) |
| serial | 0 panics, the two self-test isolation kills of the boot, 1 watchdog line (as in the load-test soaks) |

## Open

- BUG-019 (inbound TCP synchronous, httpd under load), BUG-026, BUG-027 as
  in `../../LOADTEST-TRACKER.md`.
- `[sbx2]` and the glibc set at 1 GiB vary between runs on both trees.
- The soak drives the shell, not the desktop, and ran 1 h, not 4.
