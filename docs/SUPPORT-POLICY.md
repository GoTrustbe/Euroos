# EuroOS Hardware & Peripheral Support Policy

> Status: living document, first published with Phase 4 "Metal" (2026-07).
> Companion to `docs/HARDWARE-COMPAT.md` (the tested-hardware list) and
> `docs/SPRINT-PLAN-METAL.md` (the engineering plan).

EuroOS deliberately supports **modern hardware through a small set of class
standards**, not a thirty-year catalogue of per-vendor drivers. This is both an
engineering choice (a small, auditable driver base is a security property) and a
sovereignty choice (no inherited binary-blob dependencies). This document states
what that means in practice: what we support, what we deliberately do not, and
how a peripheral gets onto the supported list.

## 1. Supported by class standard

One from-scratch driver per standard covers essentially every machine built
since roughly 2015. Each is verified in the QEMU q35 "metal matrix"
(`scripts/run-metal-matrix.py`) and, where possible, on real hardware reported
through the `hwprobe` command.

| Domain | Standard | Status |
|--------|----------|--------|
| Boot | UEFI + GOP framebuffer | ✅ |
| PCIe config | ECAM (ACPI MCFG), legacy ports fallback | ✅ |
| Storage | NVMe 1.4 (PRP lists, MSI-X) | ✅ |
| Storage | AHCI/SATA (LBA48 DMA) | ✅ |
| USB | xHCI (USB 3) + hubs | ✅ |
| Input | USB HID (boot protocol + report descriptors) | ✅ |
| Network | Intel e1000/e1000e (gigabit) | ✅ |
| Network | USB CDC-ECM (USB ethernet, phone tethering) | ✅ |
| Audio | Intel HDA | ✅ |
| Platform | ACPI (MADT, FADT, S5, SCI power button) | ✅ |
| Trust | TPM 2.0 over TIS (MMIO 0xFED40000) | ✅ |
| Storage (foreign) | FAT/exFAT/ext/NTFS(read)/SMB/NFS | ✅ |

## 2. Peripherals are network-first ("no driver, a protocol")

Where the industry moved a whole peripheral class onto the network, EuroOS
follows: we speak the **protocol** and ship **no device driver at all**. This is
the same shift Microsoft made when it removed third-party printer drivers in
favour of IPP. It is the cleanest possible sovereignty story — an open,
inspectable protocol over TCP/TLS instead of a vendor blob in the kernel.

| Peripheral | Protocol | Status |
|------------|----------|--------|
| Printers | IPP Everywhere (RFC 8010/8011) over HTTP/HTTPS | ✅ core + e2e |
| Scanners | eSCL / AirScan (mDNS + REST) | ⬜ planned |
| File shares | SMB2 / NFSv3 | ✅ |
| Discovery | mDNS / DNS-SD | ✅ |

If your printer or scanner supports the driverless standard (almost all sold
since ~2017 do), it works over the network with no EuroOS-specific driver.

## 3. Deliberate non-goals

These are **not bugs or gaps to be filed** — they are scope decisions. We would
rather do a small set of things correctly and auditably than support everything
poorly.

- **Legacy boot**: no BIOS/CSM. UEFI only.
- **Legacy storage/buses**: no PATA/IDE, no floppy, no parallel or RS-232
  peripherals, no ISA.
- **Legacy graphics**: no VGA text mode; GOP framebuffer + software rendering.
- **32-bit x86**: x86-64 only (ARM64 is a future port, not legacy).
- **GPU 3D acceleration**: no amdgpu/i915-class drivers. UEFI GOP + software
  rendering on real hardware; virtio-gpu in VMs. We do not claim 3D.
- **Non-standard sound cards**: HDA and USB audio only.

## 4. Deferred, with a reason (not refused, just not yet)

These are wanted but honestly out of reach today; each has an interim answer.

- **Wi-Fi radios** (Intel iwlwifi-class): require vendor firmware blobs and
  regulatory handling — the one place a from-scratch stack cannot be sovereign
  today. **Interim:** wired Ethernet, or USB tethering (CDC-ECM) over our own
  xHCI — both supported now. The `eurowifi` WPA/PTK protocol core is
  host-tested and honestly labelled until a radio driver exists.
- **Embedded-Controller battery** (`_BST` over EC fields): most laptops report
  battery through an EC. The ACPI *decode* and device discovery are done and
  host-tested; the live EC driver is deferred. Battery status is reported when
  statically evaluable, and "reading unavailable (EC driver deferred)" when not.
- **Realtek RTL8125 (2.5G)** and other NICs with no QEMU model: written against
  datasheets, gated behind real-hardware validation via `hwprobe`.
- **Lid switch, backlight, Bluetooth, S3/S0ix sleep, USB4/Thunderbolt tunneling**:
  later phases.

## 5. How a device gets onto the supported list

1. Boot EuroOS on the machine (the installer writes bootable USB media).
2. Run `hwprobe` in the shell. It prints a copy-pasteable inventory: the config
   mechanism (ECAM/ports), ACPI summary, every PCI function with its
   capabilities and the driver that claimed it, the storage inventory, and a
   `driven/present` summary.
3. Submit that block to `docs/HARDWARE-COMPAT.md` (the HCL).

A device is "supported" when it is driven by an in-tree class driver AND has a
real-hardware or metal-matrix report to back the claim. We do not list hardware
we have not seen work — under-claiming is policy.
