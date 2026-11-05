# Inbound TCP without waiting, bounded connect (BUG-019, BUG-026): proof (2026-11)

Branch `feature/net-async` on `feature/app-control` (`1b022ea`, which is
`bb2e9d2` plus two `.gitignore` lines from the nightly sync). What changed
and why is in `../../LOADTEST-TRACKER.md` (BUG-019, BUG-026) and in the
commit messages. "Base" below is `bb2e9d2`, built and booted the same way.

Serial logs (`*.log`) are kept next to this file on the build server and are
not committed; the `.markers` files hold the `[net-async]`, `[udrv-*]`,
`[lk]`, `[smp]`, `[kwork]` and `[net]` lines. The CSVs are the harness's
per-command results.

## Causes

**BUG-019.** A SYN for the background httpd or a LISTEN socket was served on
the spot inside `service()`, which the desktop loop calls every iteration:
SYN-ACK, then polling the NIC for the ACK and the request (`SPINS` = 4 M
iterations per look, up to 40 looks). One connection at a time, and the
console, the compositor and the watchdog waited for it.

**BUG-026.** Every HTTP/1.0 fetch left the server side in FIN-WAIT-2: the
server closes first, and `TcpConn::close()` only sent our FIN while the
connection was still open. slirp keeps such a control block for 10 minutes
(`tcp_maxidle`). A later connect that drew the same random ephemeral port
(1 in ~16 000 per lingering block; hundreds linger after a few minutes of
fetches) met that block. libslirp then either drops the SYN without an
answer (its sequence number lies left of `rcv_nxt` and the segment has no
ACK flag: `goto drop`) or answers with a bare ACK (right of the window:
`dropafterack`). The first case cost four SYN rounds of `SPINS`×3 spins each,
~21 s per round under KVM: the 84–85 s blocks. The second case failed all
four rounds in 13 ms ("no connection"). Both cases appeared about equally
often in the load test, as the two outcomes predict.

## Fix, in short

- Per-connection receive queues `(local port, peer, peer port)`, registered
  before the first segment leaves; a SYN only creates a half-open entry
  (`euronet::tcpopen::SynQueue`, bounded, SYN-ACK retransmission 1/2/4 s);
  `passive::step` completes handshakes, answers httpd requests, retires
  connections and never waits. accept() sleeps and is woken.
- Our FIN goes out after the peer's FIN too; ports avoid ports in use and the
  last 1024; SYN-SENT follows RFC 793 (stale ACK → RST at SEG.ACK, SYN again;
  acceptable RST → refused at once); the handshake is bounded in time
  (1 s + 2 s, then a fresh port and ISN, 1 s + 2 s); a spinning wait is also
  bounded by HPET time.

## Build server (QEMU 10.2, TCG)

As in `../integrate-loadtest-2026-11-03/`: `-machine q35 -cpu
qemu64,+smep,+smap`, selftest image, `-nic user,model=e1000e`, each boot
until 120 s after `[desktop] interactive loop started`, two QEMUs at a time
(`nice`), each stopped by its own PID. Compared per marker with
`scripts/markers-compare.py` (`comparison.txt`): the user and combined runs
against the logs of `../integrate-loadtest-2026-11-03/`, the
`net.driver=kernel` runs against base boots made the same day (that proof
has no kernel-driver run).

| Run | Result |
|---|---|
| `user-smp4-iommu`, `user-smp1-iommu`, `user-smp4-noiommu`, `user-smp1-noiommu` (512 MiB) | no marker worse; new `[net-async]` ×3 OK |
| `kernel-smp4`, `kernel-smp1` (`net.driver=kernel`) | no marker worse than base; `[net-async]` OK |
| `combined-smp4-iommu-1g`, `combined-smp1-iommu-1g` | no marker worse; `[sbx2]` FAIL → OK in smp4 (it varies at 1 GiB on both trees, see the earlier proof) |

`[lk] boot-total order inversions=0 task-lock-with-IF0=0` in every run.

```
[net-async] inbound httpd (synthetic peers, TEST-NET-1): 16 concurrent clients: 16 SYN-ACKs from one service() pass, 16 answered, 16 closed, 0 half-open + 0 open left, passes 2117/1533/2307 us (longest 2307 us) → OK ✓
[net-async] listen: backlog 4, 6 SYNs: 4 half-open + 2 dropped, then 6/6 accepted after the retransmitted SYNs → OK ✓
[net-async] connect: answered 1006 us; half-open peer (stale ACK): RST at its ACK sent, established in 186 us; silent drop: fresh port after 3001 ms (2 ports); our FIN after the peer's: sent; bound 6 s → OK ✓
```

(`net.driver=kernel`, smp4. With the userspace driver the first httpd pass
takes ~0.87 s under TCG: every legacy-queue read polls e1000d's IPC port, and
the NIC poll yields once per tick.)

One `user-smp4-iommu` boot of this branch stopped during the boot self-tests
(after `[glibc] gscm`, before the desktop; `tcg-user-smp4-iommu-hang1.log`,
not committed). Repeated three times next to the base: this branch 3 of 3 to
the desktop, the base 2 of 3 (its first boot stopped the same way, after a
`[sched-guard]` line: `tcg-base-user-smp4-iommu-hang.log`). Over all
`user-smp4-iommu` boots of this branch: 5 of 6. Not a regression of this
branch; open (below).

Host tests: `cargo test -p euronet` 72 passed (13 new in `tcpopen`).

## Dell R630 (KVM)

The branch and the base carried as git bundles into worktrees under
`/srv/euroos/`, built there with the host's throwaway dev key (glibc test
binaries re-signed locally, not committed). `scripts/loadtest.py --kvm --smp 4
--mem 4G --nic e1000e --restrict` (SLIRP `restrict=on`: no route out; hostfwd
and guestfwd still work), each VM pinned to its own 4 host CPUs, no VFIO
device. Files: `r630-b-*` base, `r630-n*-*` this branch.

### BUG-019: four host clients on the httpd, console checked every second

`--hostfwd P --hammer 4 --scenario inbound --duration 300`: one `echo` a
second must answer within 2 s.

| | base, `net.driver=user` | branch, `user` | base, `kernel` | branch, `kernel` |
|---|---|---|---|---|
| httpd answered / failed | 64 / 51 | **4 174 / 3** | 83 / 880 | **102 447 / 3** |
| client latency p50 / p99 | 821 / 1 402 ms | 270 / 751 ms | 7 / 1 106 ms | 9.9 / 21.6 ms |
| console | HANG: silent 120 s at `echo` #18 (t = 137 s) | 272 echoes, median 121 ms, p99 271 ms, max 633 ms | HANG at `echo` #8 (t = 124 s) | 294 echoes, median 10 ms, p99 130 ms, max 440 ms |
| watchdog lines | 1 | 0 | 1 | 0 |

The 3 failures per branch run are the clients' first attempts: the hammer
starts with the console, before `httpd on`. `scripts/loadtest-accept.py` (listen/accept through `tcpserve`,
6 rounds): base 6/6 at 0.24–0.27 s, branch 6/6 at 0.13–0.18 s.

### BUG-026: sequential outbound fetches

`--host-http --scenario net` (sequential `fetch 10.0.2.100`, then 20
downloads of 200 KB).

| | fetches | failed | median | p99 | max |
|---|---|---|---|---|---|
| base, `kernel` | stopped at 178 | 1 fast (10 ms, #119), then HANG 60 s (#178) + watchdog | 128 ms | 556 ms | > 60 s |
| branch, `kernel` | 400 | 0 | 126 ms | 446 ms | 558 ms |
| base, `user` | 300 | 3 (12.3 s, 0.6 s, 0.7 s) | 530 ms | 963 ms | 12.3 s |
| branch, `user` | 300 | 0 | 576 ms | 954 ms | 1.0 s |
| base, `user` + IOMMU | 300 | 2 (12.3 s each) | 532 ms | 1 197 ms | 12.3 s |
| branch, `user` + IOMMU | 300 | 0 | 581 ms | 934 ms | 1.25 s |

200 KB downloads: ~7.9 s median with the userspace driver on both trees.

With the userspace driver the median fetch is ~50 ms slower. An A/B build
without the FIN after the peer's FIN (`r630-ab-nofin-out-user`) had a median
of 531 ms against 581 ms for the branch (`r630-n3b-out-user`, 200 fetches
each); the kernel driver shows no difference (126 vs 128 ms). So it is the
extra frame through e1000d, not the stack's work; the FIN stays (it is what
removes the lingering peer state). Absorbing the peer's ACK of that FIN
instead of answering it with a RST (the branch's last commit, `r630-n4-out-user`)
did not change the median. Open below.

### Soak, 1 h, with inbound load

`--smp 4 --mem 16G --disks 8G --host-http --hammer 4 --procs 32 --scenario
soak --duration 3600 --boot-timeout 900`, i.e. the soak of
`../integrate-loadtest-2026-11-03/` plus four host clients on the httpd for
the whole hour (`r630-n4-soak*`).

| | |
|---|---|
| commands | 1 747, all ok; `completed clean` |
| tasks / files / kernel stacks / process pool | 55 / 186 / 0 / 887 296, flat from first to last sample |
| free frames | 2 964 990 → 2 981 630 (31 zombies of the first wave reaped) |
| kernel heap | 147.9 → 149.5 MB (+26 kB/min, BUG-027; +31 kB/min in the base's soak) |
| httpd during the soak | 4 384 answered, 641 failed, p99 9.6 s |
| serial | 0 panics, the boot's self-test kills, 1 watchdog line during `churn fs 1` (a 28 s synchronous file-system command, as in every earlier soak) |

The httpd's failures in the soak fall into the shell's long synchronous
commands (`churn fs`, `truncate`/`sha256sum` of 8 MiB, `spawn`): they hold
the desktop loop, which also drives the inbound connections. The network
path no longer blocks the loop; other synchronous work still does.

## Seen on the base, not caused here

- R630, base image, `net.driver=user`: `KERNEL PANIC` at
  `kernel/src/sched.rs:755` ("index out of bounds: the len is 256 but the
  index is 5242243857361403874": `s.current` garbage) during the boot, after
  the desktop apps came up (`r630-b2-out-user.txt`). Once in the base boots of
  this work; not seen on the branch.
- TCG, base and branch: an occasional boot that stops during the self-tests
  (above).

## Open

- The ~50 ms extra per sequential fetch with `net.driver=user`
  (one frame more per connection through e1000d).
- Inbound connections are still driven from the desktop loop: a long
  synchronous shell or file-system command delays them (soak above).
- The boot stops in the self-tests (TCG, both trees) and the `sched.rs:755`
  panic of the base.
- BUG-027 (heap growth) unchanged.
