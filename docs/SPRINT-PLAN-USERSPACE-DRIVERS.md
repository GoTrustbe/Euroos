# Sprint plan: the first driver out of the kernel (userspace e1000)

Status: ACTIVE, started 2026-11-02 on `feature/userspace-drivers`. Verified in
QEMU under TCG only (no KVM on the build server); real hardware not yet.

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

The 82574L gets one MSI-X vector (`0x4C`) routed to the BSP. The handler
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
`net.driver=user` selects the userspace driver, `net.driver=kernel` (the
default, and what a missing file means) keeps the in-kernel e1000.
`scripts/build.sh` writes the file when `EUROOS_BOOTOPTS` is set.

The default stays `kernel` for now: the dev self-test suite brings the
network up before the scheduler exists, which a userspace driver cannot do.
The userspace driver becomes the default when it is equal in function
(early-boot networking moved after the scheduler, or the suite adapted) and
in performance. Limitation: the option file is not signed; it can only pick
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

## Log

- 2026-11-02: plan written; Redox `base` and `kernel` studied.
