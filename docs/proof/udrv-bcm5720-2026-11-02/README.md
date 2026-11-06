# Proof: bcm5720d on a real BCM5720 (2026-11-02)

Branch `feature/udrv-bcm5720`. Plan: [`../../SPRINT-PLAN-BCM5720.md`](../../SPRINT-PLAN-BCM5720.md).

## Setup

- Machine: Dell PowerEdge R630, 2x Xeon E5-2630 v4, VT-d on. Four onboard
  BCM5720 ports (two chips). The two ports of the host's own networks were not
  touched. Port 3 (`0000:02:00.0`) and port 4 (`0000:02:00.1`, same chip, same
  IOMMU group) were unbound from Linux `tg3` and bound to `vfio-pci` for the
  test (non-persistent, given back to `tg3` afterwards). No cable on either.
- Chip: `14e4:165f`, chip id `0x05720000` (5720 A0), Dell subsystem `1028:1f5b`,
  bootcode `FFV21.40.21 bc 5720-v1.39`, APE NCSI firmware 1.5.12.0.
- EuroOS image built with `EUROOS_BOOTOPTS="net.driver=user" ./scripts/build.sh release`.
- VM (QEMU 10.2.1, KVM):

```
qemu-system-x86_64 -machine q35,accel=kvm -cpu host,host-phys-bits-limit=39 -m 1024 -smp 2 \
  -bios /usr/share/ovmf/OVMF.fd -drive format=raw,file=eurokernel.img \
  -device intel-iommu,intremap=off,caching-mode=on \
  -device vfio-pci,host=0000:02:00.0 -nic none \
  -display none -serial file:boot.log -no-reboot
```

`host-phys-bits-limit=39`: without it OVMF puts the 64-bit BARs at
`0x3800_0000_0000`, above the 512 GiB EuroOS identity-maps, and the grant is
refused (`AboveIdentity`, seen in the first run).

## EuroOS serial log (every line of the driver and its supervisor, in order)

```
[isolation] ring-3 page fault addr=0x100000 code=PageFaultErrorCode(PROTECTION_VIOLATION | USER_MODE) -> process pid 10 (task 12) TERMINATED
[udrv-bcm] Broadcom BCM5720 (14e4:165f) at 00:02.0: driven from ring 3 by bcm5720d
[iommu] runtime domain bcm5720d (id 8): 00:02.0 -> 64 page(s) at 0xb780000, nothing else
[udrv] bcm5720d started: pid 135168, task 27, profile driver · BAR0 0x7000000000+0x10000 · BAR2 (APE) 0x7000010000+0x10000 · DMA 0xb780000+0x40000 (64 pages) · IRQ MSI-X 0x4d -> EuroIPC · IOMMU own domain (DMA confined)
[udrv-bcm-grant] bcm5720d pid 135168: BAR0 0x7000000000+0x10000 mapped=true, BAR2 (APE) 0x7000010000+0x10000 mapped=true, a page next to either=false; DMA 64 pages mapped=true, page after=false; MSI-X table (BAR4)=false, LAPIC=false, kernel code=false → OK (exactly the granted ranges) ✓
[bcm5720d pid 135168] up in 100 ms: BCM5720 chip 0x05720000, PCI fn 0, APE NCSI 1.5.12.0, bootcode ASF on, DMA confined by the IOMMU, IRQ MSI-X
[bcm5720d pid 135168] MAC 80:18:44:f2:71:3a from the bootcode SRAM mailbox; NVRAM (offset 0x7c) says the same
[udrv-bcm-iommu] device 00:02.0 has its own VT-d domain 'bcm5720d' with 64 page(s) = the DMA window, nothing else → OK ✓
[bcm5720d pid 135168] no link (PHY reports link down: no cable or no partner)
[bcm5720d pid 135168] PHY loopback: 16 sent, 16 back, 16 byte-identical
[udrv-bcm] chip 0x05720000 (5720 A0), APE firmware NCSI v1.5.12.0, bootcode handshake done (ASF on); MAC 80:18:44:f2:71:3a from the bootcode SRAM mailbox, NVRAM agrees; PHY id 0362:5f60; link DOWN (no cable or no partner) → OK ✓
[bcm5720d pid 135168] MAC-internal loopback: 4 sent, 0 back, 0 byte-identical
[udrv-bcm-loop] PHY loopback (1000/full): 16 frames out through the send ring, 16 back through the return ring, 16 byte-identical → OK (TX DMA read + RX DMA write through the IOMMU) ✓
[udrv-bcm-loop] MAC-internal loopback: 4 sent, 0 back (Broadcom deprecates it on chips with a CPMU; Linux tg3 skips it on the 5720) — informational
[udrv-bcm-irq] MSI-X vector 0x4d: 1 interrupt(s) raised by the chip, 1 delivered to bcm5720d as EuroIPC Irq messages → OK ✓
[udrv-bcm-net] no link on this port: DHCP and HTTP not attempted ⬜
[bcm5720d pid 135168] DMA escape attempt: RX buffer at 0x1d9000 (66 loopback frames sent, 65 back), TX from 0x1da000 (Err(TxTimeout))
[bcm5720d pid 135168] re-initialised after the escape: PHY loopback 8/8 byte-identical
[udrv-bcm-dma] malicious bcm5720d: RX buffer aimed at kernel canary 0x1d9000, TX from kernel page 0x1da000 · loopback frames 66 sent, 65 back (the aimed one lost) · TX from the secret page completed=false · canary changed=false · guest vIOMMU fault records 0 (the host VT-d refuses VFIO DMA; see its DMAR log) · after a core reset: PHY loopback 8/8 → OK (device DMA confined to the driver's own window) ✓
[bcm5720d pid 135168] crash requested by the self-test: faulting now
[isolation] ring-3 page fault addr=0x1000 code=PageFaultErrorCode(PROTECTION_VIOLATION | CAUSED_BY_WRITE | USER_MODE) -> process pid 135168 (task 27) TERMINATED
[udrv] bcm5720d pid 135168 died (memory isolation: access 0x1000 denied) -> device quiesced: bus-master off, IOMMU domain revoked, 64 DMA pages freed; restart at tick 452 (death #1)
[iommu] runtime domain bcm5720d (id 8): 00:02.0 -> 64 page(s) at 0x1cb000, nothing else
[udrv] bcm5720d started: pid 135169, task 28, profile driver · BAR0 0x7000000000+0x10000 · BAR2 (APE) 0x7000010000+0x10000 · DMA 0x1cb000+0x40000 (64 pages) · IRQ MSI-X 0x4d -> EuroIPC · IOMMU own domain (DMA confined)
[bcm5720d pid 135169] up in 100 ms: BCM5720 chip 0x05720000, PCI fn 0, APE NCSI 1.5.12.0, bootcode ASF on, DMA confined by the IOMMU, IRQ MSI-X
[bcm5720d pid 135169] MAC 80:18:44:f2:71:3a from the bootcode SRAM mailbox; NVRAM (offset 0x7c) says the same
[bcm5720d pid 135169] no link (PHY reports link down: no cable or no partner)
[bcm5720d pid 135169] PHY loopback: 16 sent, 16 back, 16 byte-identical
[bcm5720d pid 135169] MAC-internal loopback: 4 sent, 0 back, 0 byte-identical
[udrv-bcm-crash] bcm5720d pid 135168 killed by a real fault (memory isolation: access 0x1000 denied); kernel kept running; restarted as pid 135169: chip reset again, MAC 80:18:44:f2:71:3a, PHY loopback 16/16 → OK (driver crash = process exit, chip back) ✓
[udrv-bcm] userspace bcm5720d: pid 135169 task 28 (starts 2, deaths 1), irq 5
```

No `panic` in the log. The other markers of the dev self-test suite were
compared with the same VM booting the branch point (`813e627`) without this
work; see the sprint plan log.

## Host side, the same run (Linux `dmesg` of the R630, the physical VT-d)

```
DMAR: [DMA Write NO_PASID] Request device [02:00.0] fault addr 0x1d9000 [fault reason 0x05] PTE Write access is not set
DMAR: [DMA Read NO_PASID] Request device [02:00.0] fault addr 0x1da000 [fault reason 0x06] PTE Read access is not set
```

`0x1d9000` is the kernel canary the malicious driver aimed an RX buffer at
(the chip's write is refused), `0x1da000` the kernel page it tried to transmit
from (the read is refused). With VFIO behind QEMU's vIOMMU (caching mode) QEMU
maps only what the guest maps, so the physical IOMMU refuses the access and
the guest's vIOMMU records nothing (`guest vIOMMU fault records 0`).
