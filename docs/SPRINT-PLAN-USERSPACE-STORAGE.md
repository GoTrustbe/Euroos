# Sprint plan: the first storage driver out of the kernel (userspace NVMe)

Status: started 2026-11-02 on `feature/udrv-default`, after `net.driver=user`
became the default (`SPRINT-PLAN-USERSPACE-DRIVERS.md`). Verified in QEMU
under TCG only (no KVM on the build server); real hardware not yet.

## Rule for this sprint: never the root

A storage driver that is not proven must not carry the root filesystem. In
this sprint the userspace driver serves a **second disk, a data mount at
`/data`**. The root keeps booting through the kernel drivers: with
`storage.nvme=user` the in-kernel `nvme.rs` leaves the controller alone, so
the root is chosen among virtio-blk, AHCI or RAM exactly as before, and an NVMe
disk can never become the root through this path. The default stays
`storage.nvme=kernel` (the in-kernel driver, which can carry an NVMe root).

## Which controller first: NVMe, not AHCI

| | NVMe | AHCI |
|---|---|---|
| Where the root is | QEMU q35 puts the boot disk on its ICH9 AHCI controller; the test disk is a separate NVMe function | The boot medium sits on the same controller (another port): a userspace AHCI driver would share a PCI function with the root disk, and one function has one owner |
| Programming model | One BAR, an admin and one I/O queue pair, 64-byte commands, phase-tag completions, PRP data pointers | Per-port command lists, FIS receive areas and command tables with PRDTs, more state to reset after a crash |
| Interrupts | MSI-X per completion queue (QEMU's NVMe has it) | MSI or legacy INTx on q35 |
| IOMMU | QEMU's NVMe DMA goes through `intel-iommu`; the kernel's `[iommu-dma]` probe already uses it | Also translated, but shared with the root disk |
| Kernel reference | `kernel/src/nvme.rs` (admin + I/O queue, PRP, MSI-X proof) | `kernel/src/ahci.rs` |

So NVMe: its own PCI function, separate from the root disk, the smallest
command model to make host-testable, MSI-X for the IRQ-to-EuroIPC path, and
DMA the IOMMU can confine.

## How Redox does it (studied, not copied)

Studied at `gitlab.redox-os.org/redox-os/base` @ `6f881b0`,
`drivers/storage/nvmed` (MIT). No Redox code is used; register offsets and
command layouts come from the NVM Express Base Specification 1.4 (as in
`kernel/src/nvme.rs`).

- `nvmed` is started by `pcid` with a channel to its PCI function, maps BAR 0
  through it (`map_bar`, uncacheable) and allocates an interrupt vector with
  `pci_allocate_interrupt_vector`.
- Bring-up: CC.EN=0 and wait for not ready, admin queues (AQA/ASQ/ACQ),
  CC with IOSQES=6 and IOCQES=4, enable and wait for ready, then identify and
  I/O queues per namespace, under a 5-second timeout.
- Requests run on an async executor driven by the IRQ handle; the disk is
  served to the system as a `disk.*-nvme` scheme (`driver-block`'s
  `RingDiskScheme`), with zero-copy reads and writes into caller-provided
  physical memory.
- No IOMMU confinement, no restart after a crash.

Taken over as ideas: the bring-up order, and one owner per PCI function. Done
differently, the EuroOS way: a capability (device grant) instead of root,
EuroIPC messages instead of a scheme, the MSI-X table kept out of the grant,
an IOMMU domain per driver, and a supervisor that quiesces and restarts.

## Design

- `crates/euronvme` (host-tested, `no_std`, no alloc, `#![forbid(unsafe_code)]`):
  the controller logic against a small `Hal` (32-bit registers, the DMA
  window): reset, admin queues, identify controller and namespace 1, one I/O
  queue pair, reads and writes of up to 8 KiB through PRP1/PRP2, phase-tag
  completions, one command in flight. Host tests against a simulated
  controller (`sim.rs`), including a re-init of a controller a crashed
  instance left enabled.
- `crates/eurodrv`: block messages in the wire format (`BlkInfo`, `BlkRead`,
  `BlkWrite`, `BlkDone`, at most 1 KiB of data each), the self-test commands
  `CrashAfterWrites` and `DmaProbe`, `Policy::DISK` (fewer restarts, longer
  backoff than a NIC), and MMIO grants above the identity map.
- `userland/nvmed`: the driver process, the HAL over its granted windows and a
  loop over EuroIPC messages. It waits for a completion by polling and, in
  between, by receiving: an `Irq` message is the completion interrupt.
- `kernel/src/udrv_blk.rs`: the supervisor, the grant, the IRQ handler, the
  block device (`UserBlk`, EuroFS's `BlockDevice` over EuroIPC), the
  self-tests and the `/data` mount.

### The grant

- **MMIO**: only the register and doorbell pages of BAR 0. On QEMU's NVMe the
  MSI-X table and PBA lie in BAR 0 behind the registers; the kernel reads
  their offsets from the MSI-X capability and grants nothing from there on,
  so the driver cannot aim the controller's interrupts.
- QEMU places the 64-bit NVMe BAR at 768 GiB, above the 512 GiB identity map.
  The grant allows that for MMIO and the driver sees the pages at
  `HIGH_MMIO_VA` (508 GiB) of its own address space
  (`paging::map_user_4k_at`); the DMA window must stay in the identity map
  (IOVA == virtual == physical).
- **DMA**: 8 pages (queues, identify page, an 8 KiB data buffer), under VT-d
  the controller's own domain `nvmed` with exactly those pages.
- **IRQ**: an MSI-X vector from the userspace-driver pool (`udrv::alloc_vector`,
  `0x4C..=0x4F`; it was a fixed `0x4D` until the driver merge) -> EuroIPC
  notification slot 1 -> `Irq` messages.

### Requests, crash and the filesystem

A filesystem block (4 KiB) goes as four 1 KiB requests; each waits for its
`BlkDone` (sleeping, woken when nvmed sends). A request fails with an I/O
error when nvmed refuses it, does not answer within 5 s, or dies. On a death
the supervisor quiesces the controller (bus mastering off, IOMMU domain back
to the empty table) before the DMA window is freed, then restarts per
`Policy::DISK`; the new instance resets the controller (CC.EN=0) before it
sets up new queues. What is on the disk afterwards is decided by EuroFS's
copy-on-write and A/B superblock commit: a write that did not reach its
commit is not there, and nothing committed is touched.

## Steps and proofs

| Step | Host tests | Boot proof |
|---|---|---|
| 1. Controller logic | `euronvme` against the simulated controller | |
| 2. Grant, IOMMU domain, driver up | `eurodrv` grant (high MMIO) | `[udrv-blk-grant]`, `[udrv-blk-iommu]`, `[udrv-blk]` |
| 3. Raw read/write | `eurodrv` block messages | `[udrv-blk-rw]` |
| 4. IRQ -> EuroIPC | | `[udrv-blk-irq]` |
| 5. IOMMU really confines | | `[udrv-blk-dma]` |
| 6. EuroFS on it, `/data` | | `[udrv-blk-fs]`, `[udrv-blk-mount]` |
| 7. Crash in the middle of a write | `Policy::DISK` | `[udrv-blk-crash]` |

QEMU line: `-device intel-iommu,intremap=off -drive file=data.img,if=none,id=d1,format=raw -device nvme,drive=d1,serial=EUROD1`,
boot option `storage.nvme=user`, 1 GiB of RAM (with a second disk the boot
preloads the installer media, which does not fit the heap of a 512 MiB
machine; that is the installer's rule, not the driver's).

## Status per step

| Step | Status | Proof |
|---|---|---|
| 1. Controller logic | ✅ host-tested | `cargo test -p euronvme` |
| 2. Grant, IOMMU domain, driver up | ✅ QEMU | `[udrv-blk-grant]`, `[udrv-blk-iommu]`, `[udrv-blk]` |
| 3. Raw read/write | ✅ QEMU | `[udrv-blk-rw]` |
| 4. IRQ -> EuroIPC | ✅ QEMU (MSI-X) | `[udrv-blk-irq]` |
| 5. IOMMU really confines | ✅ QEMU with `intel-iommu` · ⬜ real VT-d hardware | `[udrv-blk-dma]` |
| 6. EuroFS on it, `/data` | ✅ QEMU | `[udrv-blk-fs]`, `[udrv-blk-mount]` |
| 7. Crash in the middle of a write | ✅ QEMU | `[udrv-blk-crash]` |
| Root on the userspace driver | ⬜ not in this sprint, on purpose | |
| Default for data disks | ⬜ opt-in (`storage.nvme=user`) | |

## Honest limits

- Data disks only; the root never goes through nvmed in this sprint, and the
  option is opt-in.
- No NVMe Flush command yet: `UserBlk::flush` relies on every write being
  completed by the controller before nvmed answers. A device with a volatile
  write cache could still lose acknowledged writes on power loss.
- One command in flight, 1 KiB per message, data copied through EuroIPC
  (no shared buffers): correct, not fast. The driver sleeps a tick when no
  message is pending.
- The crash test crashes the driver, not the machine: power loss is a
  different proof (EuroFS's own crash-consistency tests cover torn writes).
- No interrupt remapping (`intremap=off`); the MSI-X table is kernel-only,
  which is the mitigation, not a proof.
- QEMU/TCG only.

## Two block interfaces (since the merge with megaraidd)

megaraidd (`docs/SPRINT-PLAN-MEGARAID.md`) brought a second block-over-IPC
interface, `euromegaraid::blkif`. The two were compared when the drivers were
merged (`feature/integrate-drivers`) and kept apart:

| | nvmed (`eurodrv::proto`) | megaraidd (`euromegaraid::blkif`) |
|---|---|---|
| Data | inside the messages, 1 KiB each | in 64 KiB slots of the driver's DMA window; messages name a slot |
| In flight | one request | up to four |
| Disks | one | several logical drives (`disk` field) |
| Flush | none yet | `Flush` (controller cache) |
| After a driver crash | the request fails with an I/O error; EuroFS's CoW decides | unacknowledged chunks are replayed to the next instance; the old window stays in quarantine until it reset the controller |
| Kernel side | `udrv_blk::UserBlk` | `mrblk::MrBlock` |

Both are EuroFS `BlockDevice`s, so everything above the block layer treats
them alike. Moving megaraidd onto nvmed's messages would cost it the
multi-disk, flush and replay paths and 64x the messages; moving nvmed onto
slots changes its DMA layout and the proofs that rest on it. That is a
sprint of its own, with both drivers' crash and DMA proofs run again; the
slot model is the likelier common ground (it is the one that carries
throughput and replay).

## Log

- 2026-11-02: plan written; Redox `nvmed` studied; `euronvme` host-tested;
  `nvmed` and `udrv_blk.rs` built and verified in QEMU (TCG, 1 GiB,
  `-smp 4` and `-smp 1`, `-device intel-iommu,intremap=off`, a 64 MiB NVMe
  data disk). Proof logs: `docs/proof/udrv-default-2026-11-02/`.
- 2026-11-02: merged with bcm5720d and megaraidd (`feature/integrate-drivers`).
  nvmed's vector now comes from the shared pool, its syscalls go through the
  one dispatch in `udrv.rs`.
- 2026-11-02, a bug found while testing the merge: nvmed instances stopped
  with `controller init failed: Unexpected` (an admin completion with a
  command id it had not sent): the first instance of a TCG boot, and the
  first instances after the crash test under TCG and under KVM. The
  supervisor restarted them and the markers still passed, which is how it
  hid. Cause: nvmed reads DMA memory byte by byte in ascending order, so a
  completion posted during the read could show the new phase bit (byte 14)
  with the old, zeroed command id (bytes 12-13). `euronvme` now reads the
  phase and status half first and the id after it. On the fixed tree no
  instance failed: every `[udrv-blk-*]` marker under TCG (`-smp 4` and
  `-smp 1`), in one TCG boot with e1000d and megaraidd (`-smp 4` and
  `-smp 1`), and in two KVM boots on the R630 with all four drivers. The
  other drivers read DMA memory the same way; whether their cores read the
  ready bit before the rest of an entry is open (see the proof README).
