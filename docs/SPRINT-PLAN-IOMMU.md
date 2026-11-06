# Sprint plan: IOMMU / DMA isolation (WP2, GitHub #11)

Status: ACTIVE, started 2026-10-05. Verified in QEMU (`-device intel-iommu`);
real hardware (the NUC's VT-d) when the lab is back.

## Why

EuroOS checks capabilities at the syscall boundary, but every DMA-capable
device programs physical addresses: a buggy or malicious NVMe, NIC, USB or
audio controller can read or write any memory and bypass every capability,
signature and audit entry. Since August the kernel detects the platform IOMMU
and says plainly that DMA is unrestricted; this sprint programs the translation.

## Design

- **One DMA domain per driver.** A driver's initialisation runs under
  `iommu::dma_scope`, which traces every frame it takes from the frame allocator
  and every PCI function it claims. Those frames are the only memory its
  devices may touch; heap-backed buffers (virtio-snd, the virtio-gpu scanout)
  join their domain through `iommu::dma_add`, live once translation runs.
- **Second-level translation, identity mapped.** Per domain a 3-level (39-bit)
  or 4-level (48-bit) VT-d page table with 4 KiB read+write leaves for exactly
  the registered frames. Context entries per PCI function point at their
  driver's table; a function nobody claims gets an empty table, so its DMA
  faults. Kernel heap, page tables, process arenas, the demand pool and the
  identity store are in no table at all.
- **Faults are evidence.** `iommu::poll_faults` reads and clears the units'
  fault records and logs the device, address and direction of every refused DMA.
- **Proof at boot (dev build):** `[iommu-dma]` asks the NVMe controller to DMA
  one block into a frame that belongs to no device; the memory must stay
  untouched, a fault record must appear, and the driver's own read must still
  work.

## Honest limits

- QEMU's legacy virtio devices (the virtio-pci we drive) bypass the IOMMU
  unless `iommu_platform=on` is negotiated on modern virtio; the virtio domains
  are built and will apply once the driver speaks modern virtio. NVMe, AHCI,
  e1000, xHCI and HDA are translated in QEMU and on hardware.
- Buffers a driver allocates after `program` (USB hot-plug) are only covered
  where the driver calls `dma_add`; today that is virtio-snd and virtio-gpu.
- Interrupt remapping is not programmed (`intremap=off` in QEMU); MSI/MSI-X
  delivery is unchanged.
- One domain per driver, not per device: two virtio-blk disks share a domain.

## Log

- 2026-10-05: built and verified in QEMU (`-device intel-iommu,intremap=off`,
  `-device nvme`): `translation ENABLED: 3-level second-level paging, 6
  domain(s) [virtio-blk, nvme, ahci, nic, xhci, hda], 3 claimed PCI
  function(s) confined, 4 unclaimed function(s) get NO DMA, 15 table frames`.
  Proof: `[iommu] DMA FAULT blocked: device 00:03.0 tried to write physical
  0x1f6000 (reason 0x5)` and `[iommu-dma] ... completed=Some(true)
  memory-changed=false fault-records=1 · NVMe's own read still works=true → OK`.
  The full dev self-test suite reached the desktop with translation on
  (LINUX COMPAT 35/38, unchanged). Exit criteria 1 to 3 met in QEMU.

## Exit criteria

1. `[iommu] translation ENABLED` with every DMA driver in its own domain.
2. `[iommu-dma] ... OK`: the foreign DMA is blocked and recorded, legitimate DMA works.
3. The dev self-test suite still reaches the desktop with translation on
   (disk, network, USB, audio keep working).
4. Boot without `-device intel-iommu` is unchanged (policy Warn).
