# Sprint plan: a userspace driver for the Broadcom BCM5720 (bcm5720d)

Status: ACTIVE, started 2026-11-02 on `feature/udrv-bcm5720`. Second driver on
the userspace-driver model of `SPRINT-PLAN-USERSPACE-DRIVERS.md` (e1000d), and
the first one proven on a real chip instead of an emulated one.

## Why this chip

The BCM5720 (NetXtreme, PCI `14e4:165f`) is the dual-port gigabit LOM of a
large share of rack servers (Dell PowerEdge R630/R730/R640, HPE, Lenovo). A
server OS that cannot drive it cannot use the onboard network of those
machines. It is also a hard chip in a useful way: unlike the 82574L it has an
on-chip management processor (APE) running firmware the driver must cooperate
with, a bootcode handshake on every reset, NIC-internal SRAM for ring control
blocks, a status block the chip writes into host memory, and separate
producer/return rings. If the userspace model holds for this chip, it holds
for the class.

## Test hardware

Dell R630, four BCM5720 ports (two chips). Two ports belong to the host's own
networks and are never touched. The other two (`02:00.0`, `02:00.1`, one chip,
IOMMU group 23) are detached from Linux `tg3`, bound to `vfio-pci` for the
test (non-persistent) and given back to `tg3` at the end. `02:00.0` is passed
to a EuroOS VM under KVM with QEMU's `intel-iommu` (VT-d) in front of it, so
the EuroOS IOMMU code programs a vIOMMU whose mappings QEMU shadows into the
host's real VT-d through VFIO. No cable is plugged into these ports: link,
DHCP and HTTP cannot be proven here; the driver must say "no link" correctly,
and the data path is proven with the chip's own loopback.

## Sources (studied, not copied)

- **Linux `tg3`** (`drivers/net/ethernet/broadcom/tg3.[ch]`, GPL-2.0): read
  only to understand the hardware: register offsets, bit values, the order of
  the reset and the firmware handshake, the 5717/5719/5720 quirks. No code is
  taken over.
- **FreeBSD `bge`** (`sys/dev/bge/if_bge.c`, `if_bgereg.h` @ `142cba95`,
  BSD-4-Clause): the clearest description of the 5717+ ring control blocks, the
  APE lock protocol and the block init order. Used for hardware facts only;
  no code is derived. Sources are named in the comments where a fact is used.
- **Broadcom programmer's guide**: the NetXtreme/NetLink programmer's reference
  (`57XX-PG105-R`) was published for the older 57xx parts; there is no public
  5717/5720 edition. The 5720-specific facts come from the two drivers above
  and are checked on the real chip.
- **Redox OS** (`redox-os/base` @ `6f881b09`): `drivers/net` has e1000d,
  ixgbed, rtl8139d, rtl8168d and virtio-netd; there is no tg3/bge driver.
  Nothing to compare against for this chip.

## What the chip needs (the facts that drive the design)

Measured on the R630 before any code was written (Linux `tg3` unbound, BARs
read through sysfs):

| Fact | Value on the test chip |
|---|---|
| BARs | BAR0 64 KiB registers (+ 32 KiB NIC SRAM window at 0x8000), BAR2 64 KiB APE, BAR4 MSI-X table (17 vectors) |
| Chip id (`0xF4`, gen-2 product id) | `0x05720000` (5720 A0) |
| PCI function (CPMU status bits 31:30) | 0, so PHY address 1 |
| APE firmware | signature `APE!`, ready, NCSI, version 1.5.12.0 |
| Bootcode | `KevT` signature in SRAM, ASF enabled: new firmware handshake required |
| MAC | SRAM mailbox `0xC14` (`HK` signature) = MAC registers = `80:18:44:f2:71:3a` |
| PHY | id `0362:5f60` (BCM5720C), autoneg on, no link (no cable) |
| Core reset (`GRC_MISC_CFG` bit 0) | does **not** clear the PCI command register on this chip (checked with memory decode and bus mastering on); bootcode answers `~KevT` within ~120 ms |

Consequences:

1. **Two MMIO windows.** The registers are BAR0, but the APE locks and the
   APE driver-state block are in BAR2. With APE firmware running, a driver
   that resets the core or touches the PHY without the APE lock can collide
   with the management firmware. bcm5720d gets BAR0 **and** BAR2 (never BAR4:
   the kernel programs the MSI-X table, as for e1000d).
2. **No config-space access needed after reset.** Because the core reset keeps
   the command register on the 5720, the driver can do the whole reset from its
   MMIO window (the config-space shadow at BAR0 `0x00..0xFF` covers
   `MISC_HOST_CTRL`, `PCISTATE` and the ids). It still checks memory decode
   after the reset and fails cleanly if it was lost. No new syscall.
3. **Firmware handshake** before and after every reset: `KevT` in the
   firmware mailbox, driver state START/START_DONE (ASF new handshake), APE
   host segment and START event; UNLOAD on a clean stop.
4. **Loopback is the data-path proof.** tg3 marks the MAC-internal loopback as
   deprecated on chips with a CPMU (the 5720 has one) and tests through PHY
   loopback (BMCR loopback at 1000/full). bcm5720d tries the PHY loopback and
   reports exactly which one it used.

## Design (same model as e1000d)

| Piece | What |
|---|---|
| `crates/eurobcm5720` | host-tested, `no_std`, `#![forbid(unsafe_code)]`: register facts, APE locks, reset + handshake, block init, the three rings (standard RX producer, RX return, send), status block, PHY access, loopback test, link status. Runs against a simulated chip (`sim`) in the host tests. |
| `userland/bcm5720d` | the ring-3 process: `Hal` over its two MMIO windows and the DMA window (volatile, bounds-checked against the grant), the EuroIPC loop, `Hello`/`Rx`/`Report` out, `Tx`/`Irq`/`Crash`/`DmaEscape` in. |
| `kernel/src/udrv_bcm5720.rs` | thin glue, new file: supervisor (grant, IOMMU domain, MSI-X to EuroIPC, crash detection, quiesce, restart) and the `[udrv-bcm-*]` self-tests. Since the driver merge its vector comes from the shared pool and its syscalls go through the one dispatch in `udrv.rs` (the APE window rides along as extra info words of its grant). |

Shared files touched, kept to one-line hooks (the other teams work there):
`udrv.rs` (a syscall from a driver task without an e1000d grant is offered to
the bcm module), `interrupts.rs` (one vector), `main.rs` (mod + boot + supervise
calls), root `Cargo.toml` (workspace member), `kernel/Cargo.toml` (one
dependency), `userland/build.sh` (build + sign), `.gitignore` (the signature).
`nic.rs` is **not** touched in this sprint: without a link there is nothing to
route; the netstack hookup comes with the first cabled test.

### Grant

BAR0 (64 KiB) and BAR2 (64 KiB) mapped `USER|RW|NX|PCD|PWT` at virtual ==
physical; a DMA window of 64 pages (status block, 512-entry standard ring,
1024-entry return ring, 512-entry send ring, 64 RX and 32 TX buffers of 2 KiB);
under VT-d its own domain with exactly those 64 pages. Both MMIO windows go
through `eurodrv::grant` (the second one is validated against the same rules
and against the first window and the DMA window). The driver learns the APE
window and its options (run the loopback self-test or not) from three extra
info words after the standard `DriverInfo` block (`eurobcm5720::wire`).

### Interrupts

MSI-X entry 0 programmed by the kernel to a vector from the userspace-driver
pool (`0x4C..=0x4F`, `udrv::alloc_vector`; it was a fixed `0x4D` on the
branch) on the BSP; the pool vector's stub bumps EuroIPC notification slot 2,
the driver receives `Irq{count}`.
The chip runs in tagged-status mode with one-shot MSI off; the driver
acknowledges by writing the status tag it read back into the interrupt
mailbox (see the log: without tagged status the 5720 raises no MSI-X).

### Crash and restart

As e1000d: bus mastering off, IOMMU domain revoked and flushed, process
reaped, DMA window freed, `eurodrv::restart::Policy::NIC`, a fresh grant. The
new instance starts with a core reset, so nothing a dead instance left in the
chip's rings survives.

## Steps and proofs

| Step | Host tests | Real-chip proof (EuroOS VM, KVM, VFIO, vIOMMU) |
|---|---|---|
| 1. Plan, facts measured | | this document |
| 2. Core crate | reset/handshake (APE + bootcode), block init, rings, TX/RX, wraparound, bad lengths and indices from a lying device, loopback, link, re-init after a crash | |
| 3. Driver + kernel glue | eurodrv grant checks for the second window | `[udrv-bcm-grant]`, `[udrv-bcm-iommu]` |
| 4. Detection, MAC, link | | `[udrv-bcm]` chip id, APE firmware, MAC (SRAM mailbox and NVRAM), link state |
| 5. Interrupts | | `[udrv-bcm-irq]` MSI-X -> EuroIPC |
| 6. TX/RX loopback | sim loopback | `[udrv-bcm-loop]` frames out through the send ring, back through the return ring, bytes compared |
| 7. DMA outside the domain | | `[udrv-bcm-dma]` RX and TX aimed at kernel frames: canary intact, the host's VT-d logs the fault |
| 8. Crash + restart | restart policy | `[udrv-bcm-crash]` |
| 9. DHCP + HTTP | | only with a cable: not possible on this machine |
| 10. Docs | | HARDWARE-COMPAT, STATUS, deep reference |

## Status per step

| Step | Status | Proof |
|---|---|---|
| 1. Plan, facts measured | ✅ | this document |
| 2. Core crate | ✅ host tests (simulated chip) + the same code on the real chip | `crates/eurobcm5720` |
| 3. Driver + kernel glue | ✅ real chip (VFIO, KVM, vIOMMU) | `[udrv-bcm-grant]`, `[udrv-bcm-iommu]` |
| 4. Detection, MAC, link | ✅ real chip: chip id, APE NCSI firmware, bootcode handshake, MAC from the SRAM mailbox and the same MAC read from NVRAM, PHY id, "link down" | `[udrv-bcm]` |
| 5. Interrupts | ✅ real chip: MSI-X -> EuroIPC (needs tagged status, see the log) | `[udrv-bcm-irq]` |
| 6. TX/RX loopback | ✅ real chip: PHY loopback, every frame byte-identical · ⬜ MAC-internal loopback returns nothing on this chip (Broadcom deprecates it on CPMU chips; tg3 skips it) | `[udrv-bcm-loop]` |
| 7. DMA outside the domain | ✅ real chip: canary intact, the aimed frame lost, the send engine stops on the refused read; the host's VT-d logs `DMA Write ... fault addr <canary>` and `DMA Read ... fault addr <secret>`; a core reset brings the chip back | `[udrv-bcm-dma]` + host DMAR log |
| 8. Crash + restart | ✅ real chip | `[udrv-bcm-crash]` |
| 9. DHCP + HTTP | ⬜ no cable on the test ports; the netstack is not routed to bcm5720d | `[udrv-bcm-net]` says so |
| 10. Docs | ✅ | HARDWARE-COMPAT, STATUS, deep reference |

## Honest limits at the start

- No cable on the test ports: no link, no DHCP/HTTP proof.
- With VFIO behind QEMU's vIOMMU, a DMA outside the domain is refused by the
  host's VT-d (QEMU never created the mapping); the fault is recorded by the
  host IOMMU, not injected into the guest's vIOMMU fault registers. The guest
  proof is "canary unchanged + no frame from the aimed slot"; the host's
  `dmesg` DMAR line is the hardware proof.
- The netstack (`nic.rs`) does not route to bcm5720d yet.
- No TSO, checksum offload, jumbo frames, RSS or multiple queues.

## Log

- 2026-11-02: hardware facts measured on the R630 (table above); tg3 and bge
  studied; Redox checked (no driver for this chip); plan written.
- 2026-11-02: core crate written against the simulated chip, then run
  unchanged on the real chip from a throw-away Linux VFIO harness (not part of
  EuroOS, deleted afterwards) to iterate in seconds instead of boots. Findings
  on the real chip that changed the code:
  - **The chip caches posted RX descriptors.** Re-aiming an already posted
    standard-ring descriptor (what e1000d's DMA test does) has no effect: the
    frame still lands in the old buffer. The escape test now posts one extra
    descriptor behind the 64 real ones and drives 66 frames through.
  - **A refused DMA read stops the send engine** (every later transmit times
    out); a refused DMA write only loses that frame. A core reset recovers
    both, so the escape test re-initialises and proves the chip works again.
  - **MSI-X needs tagged status.** In non-tagged mode the 5720 never raised
    an interrupt (MSI-X enabled, vector unmasked, mailbox open). bge notes the
    same for MSI on the 5717. With `MISC_HOST_CTRL.TAGGED_STATUS` and the tag
    written back to the interrupt mailbox, interrupts arrive.
  - **MAC-internal loopback returns nothing** on this chip; PHY loopback
    returns every frame. Consistent with tg3, which skips the MAC loopback
    test on chips with a CPMU.
  - The core reset keeps the PCI command register (memory decode, bus master)
    on the 5720, so no config-space help from the kernel is needed after it.
- 2026-11-02: first EuroOS boot with the chip passed through: QEMU placed the
  64-bit BARs at `0x3800_0000_0000` (above the 512 GiB EuroOS identity-maps),
  so the grant was refused (`AboveIdentity`). Fixed in the VM, not the kernel:
  `-cpu host,host-phys-bits-limit=39` makes OVMF put them at `0x70_0000_0000`.
  On the R630 itself the BIOS places them below 4 GiB.
- 2026-11-02: verified on the real chip. QEMU 10.2 under KVM,
  `-machine q35 -cpu host,host-phys-bits-limit=39 -m 1024 -smp 2
  -device intel-iommu,intremap=off,caching-mode=on
  -device vfio-pci,host=0000:02:00.0 -nic none`, `net.driver=user`. Boot log,
  abridged (full log and the host's DMAR lines in
  `docs/proof/udrv-bcm5720-2026-11-02/`):

  ```
  [iommu] runtime domain bcm5720d (id 8): 00:02.0 -> 64 page(s) at 0xb780000, nothing else
  [udrv] bcm5720d started: pid 135168, task 27, profile driver · BAR0 0x7000000000+0x10000 · BAR2 (APE) 0x7000010000+0x10000 · DMA 0xb780000+0x40000 (64 pages) · IRQ MSI-X 0x4d -> EuroIPC · IOMMU own domain (DMA confined)
  [udrv-bcm-grant] ... a page next to either=false; DMA 64 pages mapped=true, page after=false; MSI-X table (BAR4)=false, LAPIC=false, kernel code=false → OK (exactly the granted ranges) ✓
  [bcm5720d pid 135168] up in 100 ms: BCM5720 chip 0x05720000, PCI fn 0, APE NCSI 1.5.12.0, bootcode ASF on, DMA confined by the IOMMU, IRQ MSI-X
  [bcm5720d pid 135168] MAC 80:18:44:f2:71:3a from the bootcode SRAM mailbox; NVRAM (offset 0x7c) says the same
  [udrv-bcm-iommu] device 00:02.0 has its own VT-d domain 'bcm5720d' with 64 page(s) = the DMA window, nothing else → OK ✓
  [udrv-bcm] chip 0x05720000 (5720 A0), APE firmware NCSI v1.5.12.0, bootcode handshake done (ASF on); MAC 80:18:44:f2:71:3a from the bootcode SRAM mailbox, NVRAM agrees; PHY id 0362:5f60; link DOWN (no cable or no partner) → OK ✓
  [udrv-bcm-loop] PHY loopback (1000/full): 16 frames out through the send ring, 16 back through the return ring, 16 byte-identical → OK (TX DMA read + RX DMA write through the IOMMU) ✓
  [udrv-bcm-loop] MAC-internal loopback: 4 sent, 0 back (Broadcom deprecates it on chips with a CPMU; Linux tg3 skips it on the 5720) — informational
  [udrv-bcm-irq] MSI-X vector 0x4d: 1 interrupt(s) raised by the chip, 1 delivered to bcm5720d as EuroIPC Irq messages → OK ✓
  [udrv-bcm-net] no link on this port: DHCP and HTTP not attempted ⬜
  [udrv-bcm-dma] malicious bcm5720d: RX buffer aimed at kernel canary 0x1d9000, TX from kernel page 0x1da000 · loopback frames 66 sent, 65 back (the aimed one lost) · TX from the secret page completed=false · canary changed=false · guest vIOMMU fault records 0 (the host VT-d refuses VFIO DMA; see its DMAR log) · after a core reset: PHY loopback 8/8 → OK (device DMA confined to the driver's own window) ✓
  [udrv] bcm5720d pid 135168 died (memory isolation: access 0x1000 denied) -> device quiesced: bus-master off, IOMMU domain revoked, 64 DMA pages freed; restart at tick 452 (death #1)
  [udrv-bcm-crash] bcm5720d pid 135168 killed by a real fault (memory isolation: access 0x1000 denied); kernel kept running; restarted as pid 135169: chip reset again, MAC 80:18:44:f2:71:3a, PHY loopback 16/16 → OK (driver crash = process exit, chip back) ✓
  ```

  Host side, the same run (Linux `dmesg` on the R630, the physical VT-d):

  ```
  DMAR: [DMA Write NO_PASID] Request device [02:00.0] fault addr 0x1d9000 [fault reason 0x05] PTE Write access is not set
  DMAR: [DMA Read NO_PASID] Request device [02:00.0] fault addr 0x1da000 [fault reason 0x06] PTE Read access is not set
  ```

  The guest's own vIOMMU records no fault: with VFIO behind QEMU's vIOMMU
  (caching mode) QEMU only maps what the guest maps, so the physical IOMMU
  refuses the access before QEMU sees it. The two addresses are exactly the
  guest's canary and secret page.
- 2026-11-02, nothing else moved: the same VM booted the branch point
  (`813e627`, without this work) and this branch, each with and without
  `net.driver=user`. Per mode the set of `FAILED` markers is identical (the
  signed-artefact tests that need the release key, and tests that need a NIC
  or glibc programs this VM does not provide), the marker sets are identical
  except for the new `[udrv-bcm-*]` lines in `user` mode, and no run panics.
  In `kernel` mode bcm5720d is not started at all.
- 2026-11-02, merged with nvmed and megaraidd (`feature/integrate-drivers`).
  bcm5720d had the same vector (`0x4D`), EuroIPC ports, notification slot and
  pid range as nvmed and megaraidd; it now takes its vector from the shared
  pool and its ports, slot (2) and pids from `udrv::RES`, and its own copy of
  the driver syscalls is gone (the shared dispatch serves it). On the R630
  again, through VFIO, with the proof's QEMU line: every `[udrv-bcm-*]`
  marker as on the branch, the host's VT-d log with the refused write to the
  canary and the refused read of the secret page, and `[udrv-vec]` (bcm5720d
  alone gets `0x4C`). One more boot with e1000d, nvmed, megaraidd and
  bcm5720d together: four drivers on four vectors, every marker of each.
  eno3/eno4 went back to `tg3` afterwards. Logs: `docs/proof/integrate-drivers-2026-11-02/`.
