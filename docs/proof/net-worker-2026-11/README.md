# Inbound TCP in its own kernel task (net worker): proof (2026-11-05)

Branch `feature/net-worker`, started on `feature/app-control` `e9f9a31` and
rebased onto `52755fc` (the BUG-032 fix) before delivery. "Base" below is
`52755fc` unless a section says otherwise, built and run the same way. What changed and why is in `../../LOADTEST-TRACKER.md` (BUG-034 to
BUG-036) and in the commit messages.

Serial logs (`*.log`) stay on the build server next to this file and are not
committed; the `.markers` files hold the verdict lines of each log (`[tag] …
✓ / FAILED / ⬜`, the `[lk] boot-total order` line, the `[fork] … outlived its
run` lines, the `[net-worker]` lines). Comparisons use
`scripts/markers-compare.py`. The R630 files are the harness output (`*.txt`)
and its per-command CSVs.

## What was wrong

**BUG-034.** Since BUG-019 nothing on the inbound path waits, but it still only
moved when the desktop loop called `service()` / `pump_all()`. The loop also
runs the shell, so a long synchronous shell or file-system command (`churn fs`,
`truncate`/`sha256sum` of 8 MiB, 2-40 s each under KVM) stopped the httpd and
the listen/accept handshakes for its whole length: 652 of 5 222 requests failed
in the 1 h soak of `../integrate-net-mem-2026-11/`, p99 9.5 s.

**BUG-035.** e1000d sleeps one tick when its EuroIPC port is empty, and nothing
woke it early: every frame the netstack handed it, and every received frame
announced by its MSI-X interrupt, waited up to 10 ms for the driver to look.
That is per frame of every connection, both directions: the ~50 ms that one
extra frame per fetch cost in `../net-async-2026-11/`, and most of the
distance between `net.driver=user` and `net.driver=kernel`.

**BUG-036 (harness).** `sleepwait` in `scripts/loadtest.py` was a plain sleep,
so nobody read the console socket meanwhile; QEMU's chardev buffer filled and
the guest's next UART write spun, with interrupts off, until the harness read
again. In the `inbound` scenario (`echo`, then `sleepwait 1`) that stopped the
network path for 0.8-1.0 s every 1-2 s, on the base as well (`[tick-late] +88 …
+95 ticks` after each), and was most of the `max_ms` and part of the p99 of the
earlier inbound numbers. Found by this work's own long-pass log (below): a
45 s pause stalled a pass for 44.6 s.

## Design

- **A kernel task on the BSP** (`net_worker.rs`, `sched::spawn_kernel`, nice
  -5), next to task 0 under the same scheduler, so every assumption of the
  netstack holds (BSP only, e1000d on the same core, the same locks). Not a
  kwork job on an AP: the userspace driver, `may_sleep()` and the BSP
  scheduler are all BSP-side, and smp1 has no AP.
- **A pass** takes from the legacy queue only what belongs to inbound TCP (a
  SYN for the httpd or a listener, a segment of a connection that registered
  its queue late) and ARP requests for our address; everything else goes back,
  in order, for `service()` and the blocking readers (ARP/ICMP/DNS/DHCP replies,
  the one-shot `serve`). Then `passive::step` (handshakes, httpd answers, accept
  queues), and once a tick `pump_all` (ACKs and the retransmit clock of every
  socket connection). `service()` keeps its per-frame code in
  `handle_legacy()`, shared with the worker. The desktop loop still calls
  `service()` and `pump_all()`; both contexts may run them (the passive table is
  try-locked).
- **Wake-ups.** It blocks when there is nothing to do. e1000d's frame
  deliveries wake it (`wake_rx_waiters`, from the driver's syscall); with an
  in-kernel driver it polls every tick while the httpd or a listener is on
  (`httpd on` and `listen` kick it), and virtio-net's MSI-X receive interrupt
  wakes it earlier (`rx_route_irq` → `sched::try_unblock_any`, never waiting
  for the scheduler lock); every tick while half-open entries or httpd
  connections exist (SYN-ACK retransmission, linger, expiry). An
  interrupt-time wake that finds the scheduler lock busy leaves a flag the next
  kick or the desktop loop's `worker_poke()` acts on. The e1000 stays polled
  (below: an MSI-X interrupt for it was tried and measured worse).
- **Interrupts off for a pass.** That is only safe if no lock it takes can be
  held by a task the timer parked. Socket and EuroIPC locks are `Mode::Irq`
  already, the scheduler lock is never held across a switch (a switch needs
  it); the net globals (CFG, PORTQ, NET_LEGACY, UDPQ) are now taken through
  `lk()` (IF=0 while held), and so are the firewall, the ICMP limiter and the
  e1000/virtio-net rings. Before, a syscall (IF=0) that met one of those
  locks held by task 0 (IF=1, preempted) had the same exposure, the BUG-007
  family. Lock order is unchanged; `[lk]` reports 0 inversions and 0
  task locks with IF=0 in every boot.
- **e1000d** (BUG-035): the driver's task is recorded while it sleeps in its
  receive (`RECV_SLEEPER`, interrupts off between the mark and the sleep);
  `nic_send` unblocks it, the pool interrupt stub try-unblocks it, and
  `revoke_pid` drops the mark of a dead driver. e1000d only.
- **RX_WAITER** (the loose end of `../integrate-net-mem-2026-11/`): cleared
  when a task slot is reset in `reap_dead` (`reset_task_slot`) and when a fork
  child is torn down, so a reused slot is not woken early.
- **Diagnostics.** A worker pass or a `passive::step` of 50 ms or more prints
  where the time went (first 8 each). `netstat` has a `net worker:` line:
  passes, inbound frames, longest pass, wakes by source, early wakes of e1000d.
- `sched.rs` got two additive functions (`spawn_kernel`, `try_unblock_any`);
  nothing in the scheduling path changed (BUG-032 is being looked at there).

A netstack in its own context, woken by the driver's events, is also how
Redox runs its network stack (`smolnetd`, see `docs/REDOX-REFERENCE.md`); no
code is taken from it.

## Regression marker and scenario

```
[net-worker] kernel task 22 started: inbound TCP (SYN queue, httpd, accept queues) runs here, woken by e1000d's frame deliveries (EuroIPC)
[net-worker] httpd while the boot task is busy (3 s spin, no service()/pump_all from it; synthetic peers): 8 clients, 8 SYN-ACKs, 8 answered (first after 119 ms), 8 closed, 0 half-open + 0 open left, all done after 179 ms in 13 worker passes (task 22) → OK ✓
```

The boot task spins with interrupts on and never calls `service()` or
`pump_all()`; 8 synthetic clients on TEST-NET-1 (the `[net-async]` peers) open,
send their request and close, all served by the worker alone. `[net-async]`
pauses the worker (it counts what one `service()` pass does).

`scripts/loadtest.py --scenario inbound-busy`: `--hammer` clients on the httpd
while the console runs only long synchronous commands (`churn fs 1`,
`truncate` and `sha256sum` of 8 MiB). The hammer summary now splits the
requests into those that started during a console command of ≥ 5 s ("busy")
and the rest ("idle"), in every scenario.

## Build server (QEMU 10.2, TCG)

As in `../bug032-2026-11/` and `../integrate-net-mem-2026-11/`: `-machine q35
-cpu qemu64,+smep,+smap`, selftest image, `-nic user,model=e1000e`, each boot
until 120 s after `[desktop] interactive loop started`, two QEMUs at a time,
`nice`, each pinned to its own 4 cores and stopped by its own PID
(`tcg-boot.sh`, `tcg-matrix.sh`). `net.driver=kernel` via `euroos-boot.conf`.
musl ELFs from the main checkout (no musl-gcc on this server), the dev key as a
symlink.

The branch was rebased onto `52755fc` (the BUG-032 fix) before delivery, without
conflicts; the files here are of the delivered branch (`77aeec0`) against
`../bug032-2026-11/` (`comparison-tcg.txt`). The same matrix on the branch
before the rebase, against `../integrate-net-mem-2026-11/`, is
`comparison-tcg-prerebase.txt` (20 comparisons, 0 worse, 0 changed, the
`[net-worker]` line new; 12 TCG boots and 8 megaraid boots), and once more on
the rebased branch before the last two commits (14 comparisons, the same
result).

| Runs | Result |
|---|---|
| `user-smp{4,1}-{iommu,noiommu}` (512 MiB) | all 4 to the desktop; `[net-worker]` OK, woken by e1000d, first answer 112-120 ms, all done in 172-189 ms |
| `kernel-smp4`, `kernel-smp1` | `[net-worker]` OK, the polled NIC: done in 290 / 302 ms |
| `storage-smp{4,1}-iommu-1g`, `combined-smp{4,1}-iommu-1g` | `[net-worker]` OK; `[sbx2]` OK in all four |
| `megaraid-proof.sh` with and without IOMMU (4 boots) | `[net-worker]` OK; host checks 4/4 each |

14 comparisons: 0 worse, 0 changed. Only in the new logs: `[net-worker]`
(10 boots), and in the 4 megaraid boots `[3c3]`, `[3d2]`, `[3d2-wire]`, which the
markers files of `../bug032-2026-11/` do not carry (theirs hold 45 lines, these
60; all three OK here). `[lk] boot-total order inversions=0 …
task-lock-with-IF0=0` in all 10 TCG boots. No panic, no boot that stopped.

Host tests: `cargo test -p euronet -p eurolock -p euroe1000 -p euroipc -p
eurodrv`: 143 passed (none of these crates changed).

## Dell R630 (KVM)

The branch carried as git bundles into `/srv/euroos/wt-networker`, the base into
`/srv/euroos/wt-networker-base`, both built there with the host's throwaway dev
key (glibc test binaries re-signed locally, not committed).
`scripts/loadtest.py --kvm --smp 4 --nic e1000e --restrict --nmi-on-hang`, the
branch's harness for both trees, each VM pinned to its own 4 host CPUs, no VFIO
device. "Base" is `52755fc`, "branch" the delivered `77aeec0` unless a row says
otherwise. Files `r630-base-*`, `r630-branch-*`.

### 4 clients on the httpd, 300 s (`--scenario inbound`)

One `echo` a second must answer within 2 s.

| | base, `user` | branch, `user` | base, `kernel` | branch, `kernel` |
|---|---|---|---|---|
| httpd answered / failed | 4 356 / 2 | **36 233 / 3** | 111 904 / 4 | **207 498 / 3** |
| client latency p50 / p99 / max | 270 / 351 / 1 587 ms | **5.5 / 111 / 1 091 ms** | 9.9 / 21.7 / 450 ms | **5.9 / 17.0 / 1 080 ms** |
| console `echo` median / p99 / max | 70 / 172 / 431 ms | 31 / 140 / 673 ms | 10 / 66 / 380 ms | 10 / 232 / 430 ms |
| longest worker pass | | 17.6 ms | | 15.8 ms |
| watchdog lines / panics | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |

The failures are the clients' first attempts before `httpd on`, as before. A
second kernel-driver base run gave 111 449 / 2, 9.9 / 21.5 ms.

### The httpd during long synchronous commands, 600 s (`--scenario inbound-busy`)

`--mem 16G --disks 8G --hammer 4`: the console runs only `churn fs 1`,
`truncate -s 8M`, `sha256sum` in turn (16-22 commands of ≥ 5 s, 505-559 s of
the 600).

| | base, `user` | branch, `user` | base, `kernel` | branch, `kernel` |
|---|---|---|---|---|
| httpd answered / failed | 232 / 172 | **72 646 / 10** | 237 / 172 | **234 103 / 8** |
| … during the long commands | 84 / 160, p50 6.5 s, p99 8.8 s | **63 212 / 0, p50 5.7 ms, p99 125 ms** | 76 / 162, p50 6.1 s, p99 8.0 s | **206 822 / 0, p50 10.0 ms, p99 11.4 ms** |
| … the rest | 148 / 12, p50 1.9 s | 9 434 / 10, p50 5.6 ms | 161 / 10, p50 1.5 s | 27 281 / 8, p50 10.0 ms |
| `churn fs 1` median | 26.5 s | 36.3 s | 26.9 s | 34.5 s |
| commands | 63, all ok | 51, all ok | 66, all ok | 51, all ok |
| watchdog lines / panics | 1 / 0 | 1 / 0 | 1 / 0 | 1 / 0 |

The branch's failures are the first attempts before `httpd on` (no command
was running then). `churn fs` takes ~35 % longer: the BSP answers 120-390
requests a second meanwhile instead of 0.4. The watchdog line is the desktop
loop held by `churn fs`, as in every soak; the httpd no longer depends on it.

### Soak, 1 h, 4 httpd clients the whole hour

`--mem 16G --disks 8G --host-http --hammer 4 --procs 32 --scenario soak
--duration 3600 --boot-timeout 900`, `net.driver=user`, as in
`../integrate-net-mem-2026-11/` and `../bug032-2026-11/`.

| | base `52755fc` | branch `77aeec0` | base `e9f9a31` (before the rebase) |
|---|---|---|---|
| commands | **CRASH after 1 309** (t = 2 825 s, below) | 1 663, all ok, `completed clean` | 1 720, all ok |
| httpd answered / failed | 3 340 / 519 (until the crash) | **460 905 / 12** | 4 376 / 704 |
| client latency p50 / p99 | 960 ms / 9.5 s | **4.8 / 123 ms** | 888 ms / 9.5 s |
| … during commands of ≥ 5 s | 192 / 296, p50 6.7 s (1 584 s) | **302 014 / 0, p50 4.8 ms, p99 123 ms** (2 315 s) | 308 / 384, p50 4.9 s |
| … the rest | 3 148 / 223 | 158 891 / 12 | 4 068 / 320 |
| `fetch 10.0.2.100` median / max | 1 083 / 1 322 ms | 495 / 730 ms | 1 083 / 1 293 ms |
| 200 KB downloads median | 8.3 s | 9.7 s | 8.3 s |
| `heap_used` slope after 10 min | +0.64 kB/min (until the crash) | **+0.37 kB/min** (range 96 kB; 147 956 240 → 148 002 176) | +0.44 kB/min |
| tasks / files / kernel stacks / process pool | 55 / 186 / 0 / 887 296 | 56 / 186 / 0 / 887 296, flat (the worker is the one task more) | 55 / 187 / 0 / 887 296 |
| free frames | | 2 964 981 → 2 981 621 (32 zombies of the first wave reaped) | 2 964 988 → 2 981 628 |
| serial | `[idt] GENERAL PROTECTION FAULT` in ring 0 | 0 panics, 1 watchdog line (`churn fs`), no long pass | 0 panics, 1 watchdog line |

The branch's 12 failures are the clients' first attempts before `httpd on`;
its largest latencies (5.8 s during a command, 7.1 s) are single requests in
460 905. It ran 3 % fewer soak commands and its 200 KB downloads were slower:
the BSP answered 128 httpd requests a second for the whole hour, against ~1.2
on the base. Without that load (outbound, below) the downloads are as fast as
on the base. The rebased branch before the last two commits
(`r630-branch-c920f6e-soak`): 1 617 commands all ok, 452 115 / 12, p99 123 ms,
heap +0.37 kB/min.

**The base `52755fc` crashed in its soak**: `[idt] GENERAL PROTECTION FAULT
code=0x0 @ 0x7fe46c12 cs=0x8` (ring 0) during `churn proc 4`, right after a
glibc process came up (`GLIBC-DYNAMIC-OK`), at t = 2 825 s
(`r630-base-soak-gpf-tail.txt`, full log `r630-base-soak.log` next to it on
the build server). Not this branch's code; reported to the BUG-032 team. The
branch on the same base ran the hour clean twice (`77aeec0`, `c920f6e`).

### Outbound (`--scenario net`): 200 sequential fetches, 50 downloads of 200 KB

| | base, `user` | branch, `user` | base, `kernel` | branch, `kernel` |
|---|---|---|---|---|
| fetches ok | 200 / 200 | 200 / 200 | 200 / 200 | 200 / 200 |
| fetch median / p99 / max | 601 / 955 / 1 311 ms | **436 / 968 / 970 ms** | 129 / 631 / 650 ms | 135 / 620 / 630 ms |
| downloads ok, median | 50, 7.95 s | 50, 7.78 s | 50, 258 ms | 50, 261 ms |

Waking e1000d (BUG-035) takes ~165 ms off the median fetch with the userspace
driver, more than the ~50 ms the extra FIN frame cost (`r630-branch-c920f6e-out-user`,
before the last two commits, the same: 431 ms). The rest of the distance to the
kernel driver is the shell's own waiting (`RxWait` sleeps in ticks) and e1000d
handling one message per wake. The kernel driver is unchanged.

### On the way: the 82574L's MSI-X interrupt, and the branch before the rebase

The first version of the branch gave the kernel e1000 an MSI-X receive
interrupt to wake the worker. On the R630 it cost the httpd's tail with the
shell idle (p99 21.5 → 60.6 ms, two runs each; `r630-branch-c920f6e-inbound-kernel`)
and, with the shell busy, the messages thinned out to ~33 a second and the
worker answered at p50 59 ms (`r630-branch-c920f6e-busy-kernel`); the same image
with the interrupt off and a worker polling every tick: p50 10.0, p99 11.5 ms.
So the e1000 stays polled (`77aeec0`). A desktop-loop hand-over (`yield` when
the worker is pending) was tried too and made it worse (50 365 answered); not
kept.

Before the rebase (base `e9f9a31`, branch `65ff908`, files `r630-*-e9f9a31-*`)
the user-driver numbers were the same within noise: inbound 4 364 → 38 920
answered, p50 269 → 4.8 ms; inbound-busy 240 / 175 → 80 468 / 11 (during the
commands 84 / 164 → 70 018 / 0, p99 123 ms); 1 h soak 4 376 / 704 → 451 283 / 12
(p99 9.5 s → 123 ms), heap +0.44 → +0.39 kB/min; fetch median 583 → 434 ms.

## Open

- The base `52755fc` crashed once in its 1 h soak with a ring-0 general
  protection fault during `churn proc` (above); the branch's two hour-long
  soaks on that base were clean. For the BUG-032 team.
- The desktop loop itself is still held by a long synchronous command (one
  watchdog line per `churn fs`, the console waits); only the network no longer
  depends on it. `churn fs` takes ~35 % longer while the BSP serves the httpd,
  and with the kernel driver the console's p99 went from 66 to 232 ms under
  the 4-client load (median unchanged, far inside the 2 s limit).
- With an in-kernel e1000 the worker polls every tick while the httpd or a
  listener is on, which also limits the tickless fast-forward of the launcher
  to one tick then.
- A program's own connections still move when the program (or `pump_all`,
  once a tick) polls; the worker drives the httpd and the passive opens.
- e1000d still handles one message per wake (no batching); the fetch median
  with `net.driver=user` is 436 ms against ~130 ms with the kernel driver.
- One diagnostic image of this work (the branch on `e9f9a31` plus per-frame
  timing prints, not a delivered commit) panicked once in the boot under KVM;
  the harness stopped the VM after `[PA` of the banner (it now reads 10 s
  more). That base did not have the BUG-032 fix; a rerun was clean.
