# EuroOS — Phase 4 "Metal" Sprint Plan (modern hardware support)

> **Purpose.** Take EuroOS from "runs in a VM" to "boots and is usable on a modern
> (≈2018+) laptop / desktop / NUC from a USB stick". Status labels: **✅ done ·
> 🟢 core done (host-tested + boot marker), real remainder · 🟡 partial ·
> ⬜ not started · 🔒 needs real hardware (can't be fully proven in QEMU/TCG)**.
>
> Hard rules carry over: **verify by running** and **never present mock as real**.
>
> Drafted 2026-07-13 after the DOOM milestone + BUG-010/011 interrupt-safety fixes.

---

## Strategy (the decisions, so we stop re-litigating them)

1. **Modern hardware only.** No BIOS/CSM boot, no PATA/IDE, no floppy, no parallel
   or serial-port peripherals, no 32-bit x86, no USB 1.1 companion controllers,
   no VGA text mode, no sound cards other than HDA/USB. The market since ~2015 is
   consolidated on a handful of **class standards** — one from-scratch Rust driver
   per standard covers nearly every machine:
   NVMe · xHCI/USB3 · AHCI · HID · HDA · ACPI · UEFI/GOP · TPM2 · PCIe/ECAM.
   Fewer, auditable drivers is also the sovereignty story: no thirty-year blob
   inheritance.
2. **Network-first peripherals ("the Microsoft printer move", generalized).**
   Printers = IPP Everywhere over HTTP(S) (`europrint` core exists), scanners =
   eSCL/AirScan (same pattern), storage = SMB/NFS (exists). Discovery = mDNS
   (`euromdns` exists). **No peripheral driver model at all** for this whole
   category — protocols instead of drivers, and it is already ~70% in the tree.
3. **Deliberately deferred, said out loud:**
   - **WiFi radios** (Intel iwlwifi-class): vendor firmware blobs + regulatory —
     the one place a from-scratch stack can't be sovereign today. Interim answer
     that works with what we have: **wired Ethernet + USB tethering (CDC-NCM)**
     over our own xHCI. `eurowifi` (WPA/PTK protocol core) stays host-tested and
     honestly labeled until a radio driver exists.
   - **GPU acceleration**: amdgpu/i915-class drivers are millions of lines. UEFI
     GOP + software rendering is our display story on metal (it already renders
     the whole desktop); virtio-gpu covers VMs. Revisit only for display-only
     modesetting (resolution switch) — never claim 3D.
   - **Bluetooth, Thunderbolt/USB4 tunneling, S3/S0ix sleep**: later phases.
4. **Test without owning hardware.** QEMU q35 emulates the exact modern targets:
   `-device nvme`, ICH9 AHCI, `-device e1000e`, `-device intel-hda`, xHCI + USB
   device zoo, swtpm, MCFG/ECAM. A **"metal matrix" CI job** boots the same image
   across these device sets — that is our metal proxy. Real-hardware truth then
   comes from **USB-stick boots** (installer already writes them, AG-3) reported
   via a new `hwprobe` command into `HARDWARE-COMPAT.md` (3E-7 HCL).

---

## Where we already are (QEMU-verified today)

UEFI boot + GOP (any UEFI machine) · NVMe 1.4 (polling r/w, `nvme.rs`) · xHCI +
USB-HID (kbd/mouse/tablet) + USB mass storage (BOT/SCSI) · i8042 PS/2 (what real
laptop keyboards still are) · Intel HDA (codec enum + stream DMA, LPIB-proven) ·
ACPI RSDP/MADT/FADT-S5 + HPET + IO-APIC/MSI-X + SMP AP bring-up · PCI enum (legacy
0xCF8) · virtio blk/net/gpu/snd for VMs · TPM2 over swtpm · IPP + mDNS + TLS cores.

---

## Sprints

### M-1 — PCIe done right *(foundation, small)* — ✅ DONE 2026-07-13
All four items landed (commit `pci: ECAM config access + shared capability
walker + hwprobe`): ECAM live on q35 (`[ecam] … @ 0xe0000000, port-verified`),
MSI-X + virtio on the shared walker, `hwprobe` verified end-to-end, and the
metal matrix runs 7/7 legs green (also in CI as an informational job).
| ID | Task | Status | Scope & verification |
|----|------|--------|----------------------|
| M1-1 | ECAM via ACPI MCFG | ✅ | Parse MCFG, memory-mapped config for all segments/buses; keep 0xCF8 fallback. Verify: q35 lspci-equivalent (`pci` shell cmd) identical via both paths, boot marker `[ecam]`. |
| M1-2 | Capability walker + MSI-X everywhere | ✅ | One shared helper for BAR/MSI-X/power caps (today per-driver). Verify: xhci/nvme/hda re-enumerate through it, no regressions ([q1x2]/[usb]/[snd] markers stay green). |
| M1-3 | `hwprobe` shell command | ✅ | Dump PCI inventory + which driver claimed what + firmware/ACPI ids, in copy-pasteable HCL format. Verify: output on the q35 matrix matches reality; doc how users submit it. |

### M-2 — Storage on metal — ✅ CORE DONE 2026-07-13
Commit `storage: AHCI/SATA class driver + NVMe PRP lists and MSI-X`. Matrix 7/7
with tightened legs (PRP-list self-test + MSI-X confirmation + blank-disk
AHCI write/read + boot-medium read-only proof).
| ID | Task | Status | Scope & verification |
|----|------|--------|----------------------|
| M2-1 | NVMe: MSI-X + PRP lists for >8 KiB | ✅ (single I/O queue) | 64 KiB PRP-list transfers verified; `[nvme] MSI-X delivery confirmed` after interrupts enable. Per-core queue pairs deferred until SMP scheduling needs them. |
| M2-2 | AHCI/SATA (one class driver) | ✅ (polled, no NCQ/hotplug) | `ahci.rs`: port bring-up, IDENTIFY, LBA48 DMA r/w, 64 KiB PRDT window; blank-disk write/read self-test, boot medium read-only proof. AhciBlock = EuroFS BlockDevice. |
| M2-3 | Boot-device generality | 🟡 | NvmeBlock/AhciBlock mountable + hwprobe disk inventory DONE; **root-on-NVMe/AHCI install/boot still open** (loader + installer changes). |

### M-3 — Wired network on metal *(unlocks web/VPN/update/IPP on real machines)*
| ID | Task | Status | Scope & verification |
|----|------|--------|----------------------|
| M3-1 | Intel e1000e (82574/I219 family) | ✅ (82540+82574, polled) | `e1000.rs` + `nic.rs` dispatch: the whole net suite runs on the Intel NIC (matrix leg: DHCP OFFER + ping required, DNS observed). I219/I225 unbound until real-metal hwprobe validation. MSI-X + TSO later if needed. |
| M3-2 | Realtek RTL8168/8125 (2.5G) | 🔒 | The majority of consumer boards. QEMU has no model — write against datasheet/OSS references, gate behind `hwprobe` + real-metal validation. Honest label until then. |
| M3-3 | USB CDC-NCM/ECM (USB ethernet + phone tethering) | ⬜ | Class driver over existing xHCI. Verify: QEMU `-device usb-net` end-to-end DHCP→TLS; this is also the interim "WiFi" answer on laptops. |

### M-4 — USB grown up
| ID | Task | Status | Scope & verification |
|----|------|--------|----------------------|
| M4-1 | Hub support (incl. root-hub port trees) | ⬜ | Enumerate through `-device usb-hub`; hotplug attach/detach events. |
| M4-2 | HID report protocol (report-descriptor parsing) | ⬜ | Real keyboards/mice/touchpads aren't all boot-protocol. Host-test the descriptor parser (`eurousb`), boot-verify with QEMU HID variants. |
| M4-3 | UAC2 USB audio (class driver) | ⬜ | `-device usb-audio`: stream out through `euroaudio` mixer. LPIB-style DMA-consumption proof like HDA. |
| M4-4 | xHCI robustness pass | 🟡 | Ring-full handling, stall recovery, disconnect mid-transfer; fuzz the descriptor parser in `eurofuzz`. |

### M-5 — ACPI laptop basics
| ID | Task | Status | Scope & verification |
|----|------|--------|----------------------|
| M5-1 | Battery + AC status (`_BST`/`_PSR` via euroaml) | ⬜🔒 | AML interpreter exists (`euroaml`) — wire the battery/AC methods; desktop indicator. QEMU can fake ACPI battery only partially → host-test AML against dumped real-laptop tables (users submit via `hwprobe`). |
| M5-2 | Lid/power-button GPE events | ⬜ | Fixed-event + GPE dispatch; lid → lock screen, button → shutdown flow ([s5] exists). Verify: QMP `system_powerdown` event path in q35. |
| M5-3 | Backlight + thermal read-out | 🔒 | Only where standard ACPI methods expose it; never vendor-specific EC hacks in phase 4. |

### M-6 — Trust hardware on metal
| ID | Task | Status | Scope & verification |
|----|------|--------|----------------------|
| M6-1 | TPM2 TIS/CRB over MMIO (0xFED40000) | 🟡 (swtpm ✅) | Same command layer as `eurotpm`; verify: QEMU `tpm-crb`+swtpm passthrough already green → real-metal seal/unseal via USB-stick test. |
| M6-2 | UEFI Secure Boot story | ⬜ | Document + test shim/db enrolment of our signed loader on real firmware; measured boot into PCRs feeding existing attestation ([o2]/[3d3]). |

### M-7 — Network-first peripherals, end-to-end
| ID | Task | Status | Scope & verification |
|----|------|--------|----------------------|
| M7-1 | IPP Everywhere e2e | 🟢 core | mDNS-discover a real/CUPS printer → `Get-Printer-Attributes` → `Print-Job` (PDF/PWG-raster from EuroDoc). Verify against a CUPS-IPP-everywhere instance on the host (like the SMB/NFS pattern). |
| M7-2 | eSCL/AirScan scanning | ⬜ | mDNS `_uscan._tcp` + REST/XML over our HTTP(S); scan-to-EuroFiles. Verify against `airscan` simulator/CUPS. |
| M7-3 | SUPPORT-POLICY.md update | ⬜ | Write down §Strategy above as public policy: supported classes, the deliberate non-goals, and the `hwprobe`→HCL process. |

---

## Order & gates

**M-1 → M-2 → M-3 → M-4 → M-5/M-6/M-7 (parallelizable).**
Gate to declare Phase 4 "usable on metal": one real machine in the HCL that boots
from USB to the desktop with working display (GOP), keyboard (i8042 or USB), NVMe
root, wired or tethered network, and audio — all through our own drivers, no
exceptions, honestly logged via `hwprobe`.

**CI addition (with M-1):** the q35 **metal matrix** — the release image booted
against: `nvme`, `ich9-ahci`, `e1000e`, `usb-net`, `intel-hda`+`usb-audio`,
`usb-hub`+HID zoo, `tpm-crb`. Each leg must reach the interactive loop + its
subsystem marker. This is the regression net for everything above.
