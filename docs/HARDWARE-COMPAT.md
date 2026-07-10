# EuroOS Hardware Compatibility List (HCL)

> **Status: alpha.** EuroOS today is developed and verified almost entirely under
> **QEMU/KVM with OVMF (UEFI)**. This document is an *honest* support matrix: it
> distinguishes hardware/devices that are **verified working**, those whose
> **protocol core is implemented but the last mile needs real silicon**
> (`🔒`, tracked in Phase 3B), and those **not supported**. It is a living
> document — additions require a reproducible test report (see *Reporting*).

*This is a governance deliverable (3E-7) and CRA supporting evidence — it
documents the tested envelope so operators can judge fitness for a deployment.*

## Platform

| Item | Requirement |
|---|---|
| Firmware | **UEFI** (x86-64). Legacy BIOS/CSM is not supported. |
| Architecture | x86-64 (`x86_64`). No 32-bit, no ARM/RISC-V (yet). |
| Secure boot | Boots under OVMF; production Secure Boot key enrolment is future work. |
| TPM | TPM 2.0 via the **TIS** MMIO interface (`0xFED40000`). Verified against `swtpm`. |
| Memory | 256 MiB minimum (the CI/boot profile); more for real workloads. |

## Verified working (under QEMU/KVM + OVMF)

| Class | Device / interface | How it is exercised |
|---|---|---|
| Boot | UEFI loader → A/B kernel slots, GPT + FAT32 ESP | `scripts/build.sh` + standalone-boot install (`[q1x3]`) |
| Block | **virtio-blk** (legacy + modern) | root EuroFS, install, A/B update (`[g2]`,`[q1x3]`) |
| Block | **NVMe** (admin/IO queues, MSI-X) | EuroFS on `/nvme` (`[g2]`,`[j2-blk]`) |
| Net | **virtio-net** (legacy) | live ARP/DHCP/TCP/TLS, update server (`[n2]`,`[3e2]`) |
| USB | **xHCI** + HID (keyboard/mouse) | live desktop input (`[euro]` xHCI) |
| USB | USB mass-storage auto-mount | `/usb` FAT/exFAT hot-mount (`[io-usb]`) |
| Display | virtio-gpu (modern) scanout + GOP framebuffer | live compositor (`[bb2]`) |
| Security | **TPM 2.0 TIS** (swtpm) | measured boot, seal-to-PCR, FDE enrol (`[o1]`,`[3d1]`,`[3e1]`) |
| Serial | 16550 UART COM1 (log) + COM2 (GDB stub) | boot log; `gdb` attach (`[3e5]`) |
| Timers | HPET, APIC timer, RTC | scheduling, wall clock |
| SMP | multi-core via ACPI/MADT | AP bring-up (`EK_SMP=N`) |

## Protocol core done, real silicon pending (`🔒`, Phase 3B)

These have a host-tested/in-VM implementation but are **not** verified on
physical hardware. Do not rely on them for a metal deployment yet.

| Device | What exists | What the metal needs |
|---|---|---|
| GPU (real scanout) | virtio-gpu modern transport | a physical `virtio-gpu-pci` panel + mode-set |
| Wi-Fi | protocol scaffolding | Intel AX200/210 PHY/MAC + iwlwifi firmware + SAE |
| Printer/scanner | `europrint`/IPP core | live TCP → CUPS; SANE |
| Audio | HDA/virtio-audio core | real codec + routing (`3B-7`/`3F-6`) |
| Bluetooth | — (planned) | HCI stack + pairing |
| Power/thermal | — (planned) | CPU freq scaling, ACPI battery, throttling |
| Suspend/resume | partial | S3 device save/restore |

## Not supported

- Legacy BIOS / MBR-only boot.
- Non-x86-64 architectures.
- GPUs beyond a linear framebuffer / virtio-gpu (no vendor 3D drivers).
- Thunderbolt, discrete NICs other than virtio, RAID HBAs.

## Reporting a hardware result

We want a growing, *trustworthy* HCL. To add an entry, open an issue titled
`HCL: <vendor> <device>` with:

1. the exact hardware (`lspci -nn` / `lsusb` IDs), firmware, and EuroOS commit;
2. the relevant boot markers from the serial log (`[...]` lines) and/or a
   screenshot;
3. whether it **worked**, **partially worked** (what failed), or **did not boot**.

A maintainer reproduces or accepts the report (with the log as evidence) before
the device moves into *Verified working*. Unverified claims stay in an
`HCL: reported` label, never in this table — consistent with the project rule
that nothing is presented as working until it is shown to work.

---

*Companion to [`CRA-CONFORMANCE.md`](CRA-CONFORMANCE.md) (secure-by-design
evidence) and [`SUPPORT-POLICY.md`](../SUPPORT-POLICY.md) (support period &
security updates). Last revised 2026-07-10 (Phase 3E).*
