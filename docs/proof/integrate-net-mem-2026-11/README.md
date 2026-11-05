# Integration of net-async and mem-hygiene into app-control (2026-11-05)

Branch `feature/integrate-net-mem` on `feature/app-control` (`1b022ea`; the
nightly sync had added nothing since). Merged, in this order:

| Merge | Branch | What |
|---|---|---|
| `efa29ef` | `feature/net-async` (`0d8a4c2` … `1ad51a5`) | BUG-019 (inbound TCP without waiting), BUG-026 (bounded connect, FIN after the peer's FIN), `[net-async]` self-tests, `proof/net-async-2026-11/` |
| `54d97f5` | `feature/mem-hygiene` (`00fb152` … `0adc501`) | BUG-027 (EuroFS extents hint), BUG-028 (`[sbx2]` fork children), BUG-029/030 (DMA read order in e1000d, megaraidd), BUG-031 (open), heaptrace, `--nmi-on-hang`, `proof/mem-hygiene-2026-11/` |
| `c38c057` | | loadtest: one `--restrict` option (both branches had added it) |
| (last) | | this proof, the tracker (BUG-032, BUG-033) |

Full serial logs stay on the build server next to this file (`*.log` is not
committed); the `.markers` files hold the verdict lines of each log
(`[tag] … ✓ / FAILED / ⬜`, the `[lk] boot-total order` line, the `[fork] …
outlived its run` lines). Comparisons use `scripts/markers-compare.py`.

## Conflicts

| File | Conflict | Resolution |
|---|---|---|
| `docs/LOADTEST-R630-2026-11.md` | the §5 rows of BUG-026 (net: fixed) and BUG-027 (mem: fixed) | both "fixed" rows; BUG-028 to BUG-033 added |
| `scripts/loadtest.py` | `scenario_inbound` (net) and `scenario_cmds` (mem) at the same place | both kept |
| `scripts/loadtest.py` (merged without a conflict marker) | both branches added `--restrict`; argparse refused to start (`conflicting option string`) | one definition (`c38c057`); found by the first R630 run |
| `docs/LOADTEST-TRACKER.md` | merged without conflicts | the net team's two unnumbered notes are BUG-032 and BUG-033 now |

`kernel/src/main.rs`, `kernel/src/ring3.rs`, `crates/euroe1000` merged without
conflicts (the branches touch different functions).

## Interactions checked

**e1000d receive (BUG-029) and the new TCP path (BUG-019/026).** The new
`E1000::receive` reads the status byte first and the descriptor only after DD;
what it returns goes through e1000d's IPC into the netstack's demux (per
connection queues, `passive::step`) exactly as before. With the userspace
driver on the R630 the integration gives the numbers of `feature/net-async`
alone: inbound 4 165 answered / 2 failed, p50 268 ms, p99 753 ms (branch alone
4 174 / 3, 270 / 751 ms); outbound 300/300, median 581 ms, max 1.25 s (branch
alone the same 581 ms and 1.25 s). So the read-order change neither costs nor
hides anything on the network path; the ~50 ms extra per sequential fetch
with `net.driver=user` (one FIN frame more through e1000d) is unchanged.

**Fork-child teardown (BUG-028) and socket close / `reap_dead`.**
`fork_child_teardown` (kill(2) and the new end-of-run reap) releases the
child's descriptors through `fork_child_release_fds` → `close_fd_now` →
`net::sock_close`: the connection's Drop unregisters its receive queue
(`portq_unregister`), a listener calls `passive::listener_closed`. A parent's
socket close while a child still inherits the descriptor stays deferred until
that child is torn down. Over the 1 h soak with 4 httpd clients (5 218
connections served, 191 × `spawn worker 32` and 64 × `churn proc 4`): tasks 55 → 55,
files 186 → 186, process pool 887 296 → 887 296, zombies 30 → 0; at the end of
the 300 s inbound runs `half-open 0` and 4 queues, the 4 client connections
still open (`netstat`). One loose end, not changed: a child torn
down while it slept in a network wait keeps its `RX_WAITER` flag; the next
`wake_rx_waiters` then calls `unblock_any` on that (possibly reused) slot,
which only turns a Sleeping/Blocked task Ready, i.e. at worst one early wake
of a sleep that re-checks its condition anyway. Same as on the base for
kill(2).

## Build server (QEMU 10.2, TCG)

As in `../integrate-loadtest-2026-11-03/`: `-machine q35 -cpu
qemu64,+smep,+smap`, selftest image, `-nic user,model=e1000e`, each boot
until 120 s after `[desktop] interactive loop started`, two QEMUs at a time
(`nice`), each stopped by its own PID (`tcg-boot.sh`; it also sends an NMI
through the monitor when a boot is silent 15 min or reaches no desktop, which
did not happen). `net.driver=kernel` via `euroos-boot.conf`. musl ELFs from the
main checkout (no musl-gcc on this server), the dev key as a symlink.

| Runs | Result |
|---|---|
| `user-smp{4,1}-{iommu,noiommu}` (512 MiB), `user-smp4-iommu` twice more | all 6 to the desktop; no marker worse than `../integrate-loadtest-2026-11-03/`, the mem-hygiene and the net-async logs; `[net-async]` 3/3 OK |
| `kernel-smp4`, `kernel-smp1` (`net.driver=kernel`) | no marker worse than the net-async branch and base boots; `[net-async]` 3/3 OK |
| `storage-smp{4,1}-iommu-1g` (`storage.nvme=user`) | no marker worse; `[sbx2]` OK; `[fork]`, `[glibc]`, `[linux-abi]` one FAILED line fewer than the 11-03 base (GEVFD, BUG-028) |
| `combined-smp1-iommu-1g`, `combined-smp4-iommu-1g` + `-r2`, `-r3` | the same; `[sbx2]` OK in all four |
| `megaraid-proof.sh` with IOMMU, 2 runs (4 boots) | every marker identical to 11-03 and mem-hygiene; host checks 4/4 |
| `megaraid-proof.sh` without IOMMU, 3 runs (6 boots) | identical; host checks 4/4; no hang (BUG-031 did not recur) |

32 + 20 comparisons (`comparison-tcg.txt`, `comparison-mr.txt`): 0 worse.
`[lk] boot-total order inversions=0 … task-lock-with-IF0=0` in every boot.
`[sbx2]` at 1 GiB: 6 of 6 OK. Every 1 GiB boot prints `[fork] child pid 1004
… outlived its run: ended, 96 MiB back to the pool`. No panic, no watchdog
line, and all three extra `user-smp4-iommu` boots reached the desktop (BUG-033
did not recur: 4 of 4 here).

Host tests (`cargo test`, default members): 182 suites, 1 245 tests, 0
failures (among them `euronet::tcpopen`, the byte-wise DMA tests of
euroe1000, euromegaraid and eurobcm5720, `removed_objects_leave_no_extents_behind`).

## Dell R630 (KVM)

The branch carried as a git bundle into a worktree under `/srv/euroos/`,
built there with the host's throwaway dev key (glibc test binaries re-signed
locally, not committed). `scripts/loadtest.py --kvm --smp 4 --nic e1000e
--restrict --nmi-on-hang`, each VM pinned to its own 4 host CPUs, no VFIO
device, release image unless noted. Files `r630-*`.

### Soak, 1 h, 4 httpd clients the whole hour

`--mem 16G --disks 8G --host-http --hammer 4 --procs 32 --scenario soak
--duration 3600 --boot-timeout 900`, `net.driver=user` (the default), as in
`../net-async-2026-11/` (`r630-n4-soak`). Next to it, on other cores, the same
soak on the heaptrace image with `--heaptrace 15`.

| | release image (`r630-soak-user*`) | heaptrace image (`r630-soak-heaptrace*`) |
|---|---|---|
| commands | 1 805, all ok, `completed clean` | 1 771, all ok, `completed clean` |
| `heap_used` first / at 10 min / last | 147 927 568 / 147 962 240 / 147 980 416 | 148 006 272 / 148 055 296 / 148 112 656 |
| slope after 10 min (least squares) | **+0.37 kB/min** (range 96 kB) | +0.41 kB/min (range 103 kB) |
| tasks / files / kernel stacks / process pool | 55 / 186 / 0 / 887 296, flat | the same, flat |
| free frames | 2 964 988 → 2 981 628 (30 zombies of the first wave reaped) | 2 963 832 → 2 980 472 |
| httpd during the soak | 4 570 answered, 652 failed, p50 893 ms, p99 9.5 s | 4 524 / 615, p99 9.5 s |
| fetches / 200 KB downloads | 191 ok, median 1.09 s, max 3.1 s / 39 ok | 187 / 38 ok |
| serial | 0 panics, 1 watchdog line (in `churn fs 1`, a ~26 s synchronous command, as in every earlier soak) | the same |

Heap: flat. +26 kB/min on `feature/net-async` alone, +0.4 kB/min on
`feature/mem-hygiene` alone, +0.37 kB/min integrated. heaptrace diffs since
the snapshot after 15 rounds (`r630-heaptrace-sym.txt`, raw lines in
`r630-heaptrace-raw.txt`): sum over all sites −12.7 kB, +20.7 kB, +22.0 kB,
+25.4 kB at the end (before the BUG-027 fix: +1.26 MB); no site grows with
the rounds (the EuroFS extents sites: 13 of 2 560 blocks live).

httpd: no collapse. The failures and the 9.5 s p99 are the same as in the
net-async soak (4 384 / 641, p99 9.6 s) and fall in the shell's long
synchronous commands (`churn fs`, `truncate`/`sha256sum` of 8 MiB), which hold
the desktop loop that also drives the inbound connections (open, see
`../net-async-2026-11/README.md`).

### The net team's load test: 4 clients on the httpd, 300 s

`--mem 4G --hammer 4 --scenario inbound --duration 300`: one `echo` a second,
each within 2 s.

| | `net.driver=user` | `net.driver=kernel` |
|---|---|---|
| httpd answered / failed | 4 165 / 2 | 101 202 / 3 |
| client latency p50 / p99 | 268 / 753 ms | 9.9 / 21.7 ms |
| console | 272 echoes, median 120 ms, p99 253 ms, max 693 ms | 294 echoes, median 10 ms, p99 111 ms, max 380 ms |
| watchdog lines / panics | 0 / 0 | 0 / 0 |
| `net-async` branch alone | 4 174 / 3, 270 / 751 ms | 102 447 / 3, 9.9 / 21.6 ms |

The failures are the clients' first attempts before `httpd on`, as in the
branch's proof.

### Outbound (BUG-026), for completeness

`--host-http --scenario net`: kernel driver 400/400 fetches, median 89 ms,
p99 514 ms, max 677 ms; userspace driver 300/300, median 581 ms, p99 770 ms,
max 1.25 s. 0 failures, no watchdog line.

Every R630 boot against the matching net-async and mem-hygiene boot
(`comparison-r630.txt`): 0 markers worse, the only difference the three
`[net-async]` lines where the mem-hygiene image had none.

## Open

- BUG-031 (one silent boot without IOMMU, mem-hygiene), BUG-033 (TCG boots
  that stop in the self-tests, `user-smp4-iommu`): no recurrence in 6 boots
  without IOMMU and 4 `user-smp4-iommu` boots here, so no NMI probe yet. They
  may share a cause (a glibc program during the boot, then silence).
- BUG-032 (`sched.rs:755`, `s.current` overwritten with what reads as x86-64
  code bytes, once on the base under KVM): not seen here.
- Inbound connections are still driven from the desktop loop (soak above);
  the ~50 ms extra per sequential fetch with `net.driver=user`.
- A fork child torn down in a network wait leaves its `RX_WAITER` flag
  (harmless early wake, above).
