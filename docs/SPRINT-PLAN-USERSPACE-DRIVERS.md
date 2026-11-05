# Sprint plan: the first driver out of the kernel (userspace e1000)

Status: ACTIVE, started 2026-11-02 on `feature/userspace-drivers`, merged with
the J1 locking work on `feature/integrate-j1-udrv`. Since `feature/udrv-default`
(2026-11-02) **`net.driver=user` is the default**; `net.driver=kernel` is the
fallback. Verified in QEMU under TCG only (no KVM on the build server); real
hardware not yet. The next driver (NVMe, a data disk) has its own plan:
`SPRINT-PLAN-USERSPACE-STORAGE.md`.

## Why

The design rule says "microkernel, drivers in userspace", but `kernel/src`
still holds every driver (e1000, AHCI, NVMe, HDA, xHCI, virtio), the
compositor and the apps. A driver bug is a kernel bug, and before the IOMMU
sprint (`SPRINT-PLAN-IOMMU.md`) any device could DMA over all of RAM. This
sprint moves one real driver to ring 3 and builds the kernel primitives every
later driver needs, so the next ones are mostly porting work.

## Which driver first, and why e1000

| Candidate | For | Against |
|---|---|---|
| **e1000/e1000e** | QEMU's default NIC on q35 (82574L, `8086:10D3`); the kernel already has a small, readable polled driver (`kernel/src/e1000.rs`, 223 lines) to compare against; a NIC is the most exposed device (it parses attacker-controlled packets); its DMA model (two descriptor rings + buffers) is the textbook case for an IOMMU domain; MSI-X on the 82574L gives a real interrupt path; the network self-tests (DHCP, DNS, HTTP) give an end-to-end proof | The boot self-test suite uses the network before the scheduler runs, so the userspace driver cannot replace the kernel driver at that point (see "Boot option") |
| virtio-net | Already the tuned path for e2e tests | Legacy virtio (what we drive) bypasses the IOMMU in QEMU, so the DMA-isolation proof would be impossible |
| AHCI / NVMe | Storage is critical | The root filesystem sits on it: a crashed storage driver needs a much bigger recovery story (in-flight writes, the page cache) before restarts are safe. Second wave. |
| HDA | Self-contained | Little security value, no end-to-end test peer |

So e1000 first: it is the most exposed, its DMA is translated by QEMU's
`intel-iommu`, and it has an end-to-end test.

## How Redox does it (studied, not copied)

Studied at `gitlab.redox-os.org/redox-os/base` @ `6f881b09` and
`redox-os/kernel` @ `1a1295b0` (both MIT). No Redox code is used in EuroOS;
register offsets come from the Intel 8254x/82574 datasheets (already used by
`kernel/src/e1000.rs`).

- `pcid` is a userspace daemon that owns PCI configuration space.
  `pcid-spawner` matches each function against a TOML driver table and starts
  the driver with a channel fd to `pcid`; a function can have one owner
  (`ENOLCK` otherwise).
- A driver maps a BAR through that channel (`map_bar`), which ends in the
  kernel's `memory:` scheme `physmap`. The gate is *being root* (`uid == 0`),
  not "this BAR only": a root driver can map any physical range.
- DMA memory is `memory:zeroed?phys_contiguous`; the driver gets physical
  addresses and writes them into descriptors. There is no IOMMU confinement on
  this path.
- IRQs are files under `irq:`; a read blocks until the line fired, a write
  acknowledges and unmasks. Drivers multiplex IRQ and scheme events with an
  event queue.
- `e1000d` publishes a `network.<pci-name>` scheme; the netstack (`smolnetd`)
  opens it and reads/writes whole frames.
- After setup the driver calls `setrens(0, 0)` (the null namespace): it can no
  longer open anything new.
- No automatic restart of a crashed driver.

What we take over as ideas: one owner per PCI function, a spawner that binds
drivers to functions, frames as whole messages between driver and netstack,
IRQs as messages the driver waits on, and "drop everything after setup".
What we do differently, the EuroOS way: no schemes and no root; a
**capability per device** that maps exactly one BAR; IRQs as **EuroIPC**
messages; DMA confined by a **VT-d domain per driver**; a **restart
supervisor** that quiesces the device before it frees anything.

## Design

### Shape

- `crates/eurodrv` (host-tested, `no_std`, `#![forbid(unsafe_code)]`): the
  device grant (what a driver may touch, with every check), the wire protocol
  between netstack and driver, the driver syscall numbers, the restart policy.
- `crates/euroe1000` (host-tested, `no_std`, `#![forbid(unsafe_code)]`): the
  e1000/e1000e logic (reset, rings, transmit, receive, interrupt causes)
  against a small `Hal` trait; host tests run it against a simulated device.
- `userland/e1000d/` (Rust, `x86_64-unknown-none`, static PIE): the driver
  process. It implements `Hal` with volatile reads/writes on its granted
  windows and runs `euroe1000` in a loop. The only `unsafe` is those
  accesses.
- `kernel/src/udrv.rs` (thin glue): the supervisor, the grant, the driver
  syscalls, the IRQ handler, the NIC backend and the `[udrv-*]` self-tests.
- Small hooks: `iommu.rs` (runtime domains), `euroipc.rs` (sender-restricted
  ports, kernel endpoint, IRQ notifications), `nic.rs` (a userspace backend),
  `paging.rs` (map one 4 KiB user page inside an identity huge page),
  `eurosandbox` (a `driver` syscall profile), `ring3.rs` (route the driver
  profile), `interrupts.rs` (one vector), `main.rs` (boot calls).

### MMIO: a capability that maps one BAR range

The supervisor builds a `DeviceGrant` for the PCI function: the BDF, BAR 0's
physical range (size probed from the BAR, page-rounded), the DMA window and
the IRQ vector. `eurodrv::grant` validates it (page-aligned, non-empty, below
512 GiB, no overlap with the DMA window, no wrap) before anything is mapped.
The driver's address space gets exactly those BAR pages as
`USER | RW | NX | PCD | PWT`, at virtual == physical (EuroOS identity
convention). Everything around them stays supervisor-only, so a pointer one
page past the BAR faults like any other isolation violation. The MSI-X table
(BAR 3 on the 82574L) is **not** granted: the kernel programs it, so the
driver cannot aim its interrupt messages at arbitrary addresses.

The grant is the capability: it lives in the kernel table keyed by the
driver's pid, is created only by the supervisor, cannot be passed on, and
dies with the process. The driver syscalls (`0x6200..`) find nothing for a
pid without a grant (`-EPERM`).

### Interrupts as EuroIPC messages

The 82574L gets one MSI-X vector (`0x4C`; since the driver merge it comes
from the userspace-driver pool, `udrv::alloc_vector`) routed to the BSP. The handler
does no allocation and takes no lock: it bumps an atomic notification word
bound to the driver's EuroIPC port and sends EOI. The driver's receive call
(`IPC_RECV`) delivers a pending notification first, as an ordinary EuroIPC
message from the kernel (`Irq { count }`), before queued data messages. The
driver reads ICR (which acknowledges the cause on e1000) and services the
rings. A device without MSI-X (the 82540EM) is serviced by the same loop on a
one-tick timeout: honest polling, labelled as such in the log.

Latency honestly: when no message is pending the driver sleeps one scheduler
tick (10 ms); the IRQ handler does not wake it early (that needs a
scheduler wake from interrupt context, which the locking work owns).

### DMA through the IOMMU, own buffers only

The supervisor allocates the driver's DMA window (32 contiguous 4 KiB pages:
two descriptor rings and 2 x 16 buffers of 2 KiB) and maps it into the
driver at virtual == physical. Under VT-d the device gets its **own domain**:
a second-level table that maps exactly those 32 pages, and the device's
context entry points at it (`iommu::attach_runtime`). Since IOVA == PA ==
the driver's virtual address, the driver writes its own pointers into
descriptors. Anything else (kernel heap, page tables, other processes,
other drivers' buffers) is in no table the device can use: the DMA faults,
the fault record is logged, the memory is untouched.

Without an IOMMU (plain QEMU, firmware with VT-d off) the driver still runs,
but its DMA is not confined and the log says so (`DMA NOT confined`). The
IOMMU boot policy (`Required`) still applies.

### Netstack <-> driver over EuroIPC

EuroNet stays in the kernel for now (moving it out is a later sprint). It
reaches the driver through `nic.rs`, which gets a fourth backend, `user`:

- `nic::send(frame)` -> `Tx(frame)` to the driver's port (sender: kernel).
- The driver sends `Rx(frame)` and `Hello { mac }` (link up) to the netstack
  port; `nic::poll_recv` takes from it. When nothing is queued it yields the
  CPU so the driver can run.
- Both ports are **sender-restricted**: the driver port accepts only the
  kernel, the netstack port only the pid that currently holds the NIC grant.
  This replaces the open "permission hook" in EuroIPC for these ports.
- Queues are bounded (drop and count on overflow), so a dead or slow driver
  cannot exhaust kernel memory.

The DHCP, ARP, DNS and HTTP code runs unchanged on top of `nic`.

### Crash and restart

The supervisor (`udrv::supervise`, called from the desktop loop and from the
self-test) notices a dead driver (the ring-3 fault path already marks the
process a zombie). Before anything is freed it **quiesces** the device:

1. PCI bus mastering off, so the device cannot start new DMA.
2. The device's context entry back to the empty (deny) table, the domain's
   mappings removed, context cache and IOTLB invalidated.
3. The NIC backend reports link down (sends fail cleanly, receives are empty).
4. The process is reaped (arena, page tables incl. the MMIO/DMA mappings);
   then the DMA window is freed.

Then it applies `eurodrv::restart::Policy` (at most N restarts per window,
with backoff) and spawns a fresh driver with a fresh grant. The new driver
resets the device (`CTRL.RST`) before it programs new rings, sends `Hello`,
and the netstack's link comes back. The kernel never runs driver code, so a
driver crash is a process exit.

### Boot option and the in-kernel fallback

`\EFI\BOOT\euroos-boot.conf` on the ESP (read before ExitBootServices):
`net.driver=user` (the default since 2026-11-02, and what a missing file
means) selects the userspace driver, `net.driver=kernel` keeps the in-kernel
e1000 (the fallback). `scripts/build.sh` writes the file when
`EUROOS_BOOTOPTS` is set. The same file takes `storage.nvme=user` and, for
debugging, `debug.schedwatch=on` (a debug-register write watch on the
scheduler state, BUG-032).

At first the default stayed `kernel`: the dev self-test suite brought the
network up before the scheduler existed. How that was solved is in "Making
`user` the default" below. A machine with a virtio-net or USB NIC binds that
in the kernel before the scheduler, as before; e1000d only takes the
82540EM/82574L. Limitation: the option file is not signed; it can only pick
between two shipped, signed code paths.

### Syscalls and profile

A driver runs under a new `eurosandbox::Profile::Driver`: the driver calls
(`0x6200` info, `0x6201` receive, `0x6202` send), plus log, clock, sleep and
exit from the Euro API. No files, no sockets, no fork, no mmap, no window.
Anything else kills the process (SIGSYS), like the `euro` profile.

## Steps and proofs

| Step | Host tests | Boot proof |
|---|---|---|
| 1. Primitives: grant + MMIO mapping, IRQ->IPC, IOMMU domain per driver | `eurodrv` (grant checks, protocol, restart policy), `eurosandbox` (driver profile) | `[udrv-grant]`, `[udrv-irq]`, `[udrv-iommu]` |
| 2. Userspace e1000 driver | `euroe1000` against the simulated device | `[udrv-e1000]` driver up, MAC, link via `Hello` |
| 3. Netstack via the driver: DHCP + real HTTP fetch | (stack unchanged) | `[udrv-net]` DHCP lease, DNS, HTTP status from example.com |
| 4. Crash test | restart policy | `[udrv-crash]` driver killed by a real ring-3 fault, restarted, DHCP + HTTP again |
| 5. IOMMU really confines | grant/window checks | `[udrv-dma]` the driver aims RX/TX DMA at a kernel canary: fault records, canary unchanged |
| 6. Docs | | STATUS, deep reference, ISOLATION-LEVELS |

QEMU line for the proofs (TCG, one instance, `-smp` at most 4):
`-machine q35 -device intel-iommu,intremap=off -nic user,model=e1000e`.

## Honest limits (at the start of the sprint)

- One driver, one device class. AHCI, NVMe, HDA, xHCI, virtio, the compositor
  and the apps are still in the kernel.
- EuroNet itself stays in the kernel; only the device driver moves.
- No interrupt remapping (`intremap=off`): MSI writes are not translated.
  The MSI-X table is kernel-only, which is the mitigation, not a proof.
- Wake-up latency is bounded by the 10 ms tick.
- QEMU/TCG only until the lab hardware is back.

## Status per step

| Step | Status | Proof |
|---|---|---|
| 1. Primitives: grant + MMIO mapping, IRQ->IPC, IOMMU domain per driver | ✅ QEMU | `[udrv-grant]`, `[udrv-irq]`, `[udrv-iommu]` |
| 2. Userspace e1000 driver | ✅ QEMU (82574L, MSI-X) · ⬜ 82540EM interrupts (polled) | `[udrv-e1000]` |
| 3. Netstack via the driver: DHCP + HTTP | ✅ QEMU, kernel-side users and processes (musl background processes, scheduled glibc processes) | `[udrv-net]`, `[net-late]`, `[exec] /bin/msock ...`, `[udrv-linux]` |
| 4. Crash test | ✅ QEMU | `[udrv-crash]` |
| 5. IOMMU really confines | ✅ QEMU with `intel-iommu` · ⬜ real VT-d hardware | `[udrv-dma]` |
| 6. Docs | ✅ | STATUS, deep reference, ISOLATION-LEVELS |
| Make `user` the default | ✅ QEMU (`-smp 4` and `-smp 1`) | see "Making `user` the default"; logs in `docs/proof/udrv-default-2026-11-02/` |

## Log

- 2026-11-02: plan written; Redox `base` and `kernel` studied.
- 2026-11-02: built and verified in QEMU (TCG, `-m 512M`, `-device intel-iommu,intremap=off`,
  `-nic user,model=e1000e`, `net.driver=user`). Boot log, abridged:

  ```
  [iommu] runtime domain e1000d (id 8): 00:02.0 -> 32 page(s) at 0x1cb000, nothing else
  [e1000d pid 131072] up: 8086:10d3 MAC 52:54:00:12:34:56, MSI-X, DMA confined by the IOMMU
  [udrv-grant] ... page before=false, page after=false; ... LAPIC=false, kernel code=false → OK (exactly the granted ranges) ✓
  [udrv-iommu] device 00:02.0 has its own VT-d domain 'e1000d' with 32 page(s) = the DMA window, nothing else → OK ✓
  [udrv-net]   HTTP GET http://example.com/ -> 888 bytes, HTTP/1.1 200 OK
  [udrv-net] netstack over EuroIPC through e1000d: DHCP + ARP + DNS + HTTP ... → OK (the kernel e1000 driver was not used) ✓
  [udrv-irq] MSI-X vector 0x4c: 16 interrupt(s) raised, 16 delivered to e1000d as EuroIPC Irq messages → OK ✓
  [iommu] DMA FAULT blocked: device 00:02.0 tried to read physical 0x1fa000 (reason 0x6) — outside its domain
  [iommu] DMA FAULT blocked: device 00:02.0 tried to write physical 0x1f9000 (reason 0x5) — outside its domain
  [udrv-dma] malicious e1000d: ... IOMMU fault records: TX read 1, RX write 1 (total 2) · canary changed=false · driver still works=true → OK ✓
  [isolation] ring-3 page fault addr=0x1000 ... -> process pid 131072 (task 27) TERMINATED
  [udrv] e1000d pid 131072 died (...) -> device quiesced: bus-master off, IOMMU domain revoked, 32 DMA pages freed; restart at tick 515 (death #1)
  [udrv-crash] ... sends refused while dead=true; restarted as pid 131073 ..., DHCP + HTTP again → OK ✓
  ```

  Also run: the same image without `-device intel-iommu` (everything passes, the
  IOMMU and DMA steps report "not confined" ⬜), and the same kernel without the
  option file (in-kernel e1000 binds, same self-test results as before the
  sprint). In `user` mode the early network self-tests of the dev suite do not
  run (they come before the scheduler); the rest of the suite is unchanged.
- Lessons: the netstack waits in spin loops with iteration-count timeouts,
  which crawl when the dev suite's busy processes share the CPU (the first DMA
  probe built on `net::arp_resolve` stalled for minutes); the self-test waits in
  ticks instead, and the NIC backend yields at most once per tick. QEMU's VT-d has one fault-recording
  register: the self-test reads and clears it between the TX and RX probes.
- 2026-11-02, merge with the J1 locking work (`feature/integrate-j1-udrv`):
  EuroIPC now runs on the per-channel core `crates/euroipc` (one lock per port,
  `Mode::Irq` instead of the hand-written `without_interrupts`, notification
  slots unchanged). Wiring it found one semantic gap: the core treated
  `only_from == u64::MAX` as "unrestricted", which would have opened the
  netstack port to every sender before a driver holds the grant; fixed in the
  core with a separate restricted flag. Re-verified on the merged kernel (QEMU
  TCG, 512 MiB, `-smp 4`, `-device intel-iommu,intremap=off`,
  `-nic user,model=e1000e`, `net.driver=user`): every `[udrv-*]` marker as in
  the log above, including `[iommu] DMA FAULT blocked` for the read and the
  write, canary unchanged, and the crash/restart with DHCP + HTTP again; also
  without the IOMMU (`-smp 1`), where `[udrv-iommu]`/`[udrv-dma]` report "not
  confined" as before. Logs: `docs/proof/integration-j1-udrv-2026-11-02/`.

## Making `user` the default: where it was stuck (measured 2026-11-02, solved the same day, see the next section)

At the time the default stayed `net.driver=kernel`. What blocked the switch, precisely:

1. **The dev self-test suite needs the network before the scheduler.** The
   network block in `main.rs` (DHCP, ARP, ping, DNS, HTTP, HTTPS, IPv6) and the
   foreground runs of `/bin/msock`, `/bin/mdns`, `/bin/mtrack` happen before
   `sched::init()` and before the APIC timer starts. A ring-3 driver cannot run
   then. Measured in `user` mode: `connect()` of msock returns -1 at once (no
   network configuration yet), `[exec] /bin/msock ... exit=1`, and the markers
   `[e1000]`, `[inet]`, `[udpq]`, `[g3]` are missing; every other marker is
   the same as in `kernel` mode. Fix: move the network part of the suite after
   the scheduler and `udrv::boot` (or give it a `user`-mode variant), keeping
   the kernel-mode order identical.
2. **Musl background processes have no socket syscalls.** `bg_dispatch`
   (`ring3.rs`, run under the global `BG` lock) answers every number it does
   not know with 0. Measured with an experimental `[udrv-msock]` (patch in the
   proof folder, not merged): msock as a scheduled process over `e1000d`
   "finished" in 8 ticks with 0 frames sent and `response: 0 bytes`. So for
   this path the BG lock is not the blocker; the socket calls are simply
   absent. Fix: route the socket numbers of that path to the existing socket
   layer, outside the BG lock.
3. **The Linux-ABI socket path spins with interrupts off.** `linux_dispatch`
   (foreground programs, glibc processes) does not hold `BG`, and glibc
   processes may already yield there (`syscall_yield_ok`, used by futex and
   epoll), but the blocking calls in `net.rs` (`TcpConn::connect`/`recv`,
   `poll_seg` = `SPINS * 3` iterations per round) never yield, and the NIC
   backend only yields when interrupts are on. All ring-3 code, `e1000d`
   included, runs on the BSP, so the driver cannot run while such a call waits.
   Fix: blocking calls as loops over the existing `*_nowait` primitives
   (`connect_nowait`/`poll_connect`, `recv_nowait`, `send_nowait`) with a
   one-tick sleep between rounds when `syscall_yield_ok()`. This is code
   reading, not yet measured in `user` mode: the glibc tests need more than
   512 MiB.

All three change the boot order or the syscall path that the kernel-mode
default depends on; the switch should come with its own proof that nothing
moves in `kernel` mode (`-smp 4` and `-smp 1`) and that msock and the network
tests of the suite pass in `user` mode.

## How `user` became the default (2026-11-02, `feature/udrv-default`)

1. **The network part of the boot moved behind the scheduler** (`main.rs`,
   `net_late_suite`, marker `[net-late]`). With the userspace driver and no
   other NIC bound before the scheduler, the pre-scheduler block only notes
   that the e1000 is left to `e1000d`. After `udrv::boot` a bounded wait
   (20 s; on expiry a FAILED line and every network program reported as not
   run) checks link and DHCP, then the same probes as the kernel path run
   (ARP, ping, DNS, HTTP, HTTPS, IPv6 router discovery, `[g3]`, SMB/NFS), and
   `/bin/msock`, `/bin/mdns`, `/bin/mtrack` run as scheduled musl processes
   with the capabilities of the program registry, their output mirrored to
   the log (`[bg-out]`). The kernel path is unchanged: with
   `net.driver=kernel` the per-marker verdicts are identical to the J1 x udrv
   logs, `-smp 4` and `-smp 1`.
2. **Socket syscalls for background processes** (`ring3.rs`,
   `bg_net_dispatch`). Routed before the BG lock is held for them; their own
   pointer checks; `socket()` needs `CAP_NET` in the process's own capability
   set; sockets owned per pid; EuroGuard under the process's own name;
   sockets closed when the process is reaped. Unknown background syscalls
   answer `-ENOSYS` (`bg_unknown`; a whole selftest boot answered none, so
   nothing relied on the silent 0 there). A static look at the embedded
   programs shows what an interactive run could still meet: the DOOM port
   links `mkdir`, `rmdir`, `unlink`, `rename`, `fcntl`, `getuid`/`getgid`
   and friends that used to "succeed" with 0 and now fail with `ENOSYS`;
   not exercised in this boot.
3. **Blocking socket calls give the CPU away** (`net.rs`, `RxWait`; wake-up
   from `udrv` on every driver send). Details in the deep reference,
   "Userspace drivers". Effect on the BG lock: its boot-total hold-max is
   18 to 22 ms in every run, before and after (the network waits of
   background processes are outside the lock now: msock alone waits about
   130 ms for its page, while the BG hold-max over the same stretch stays
   below 19 ms); the scheduled glibc program `/bin/gnet` fetches a page with
   blocking connect/send/recv, its waits gave the CPU away and were woken
   early by e1000d (`[udrv-linux]`, a 1 GiB run; at 512 MiB glibc has no
   arena and the line says so).

Found on the way: with the userspace NIC a frame sent or received under a
socket lock is a EuroIPC message, which the J1 lock order flagged as an
inversion; the IPC classes moved inside the network classes
(`SPRINT-PLAN-FINE-LOCKING.md` §10). And both supervisors turned bus
mastering on after the driver task could already run; now it is on before.

Honest limits that remain: a foreground program (a synchronous excursion on
task 0, as the boot script runs its programs) cannot wait for the userspace
NIC, its socket calls spin; wake-up latency is a tick when no frame arrives;
QEMU/TCG only. Proof logs: `docs/proof/udrv-default-2026-11-02/`.
Not explained: two boots of this branch stopped at the chrome_crashpad step
of the glibc suite (one hang, one machine reset); neither reproduced in the
reruns or in a side-by-side comparison with `813e627`. Details in the proof
README.
