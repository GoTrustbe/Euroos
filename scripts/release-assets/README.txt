EuroOS — preview build (x86-64 UEFI)
====================================

This is an EARLY ALPHA preview of EuroOS, a sovereign operating system built
from scratch in Rust — its own kernel, filesystem, network stack, capability
security model and desktop, with no Linux or BSD underneath.

It boots to a working desktop showing a live System window (real kernel status:
scheduler, processes, SMEP/SMAP, EuroMonitor) and an interactive Terminal, with
real networking and on-disk persistence. On a machine with hardware
virtualization it boots in roughly 1–2 seconds.

This build also includes the sovereign-platform layer, all self-tested on boot:
real USB (xHCI keyboard/mouse + mass storage), Intel HD-Audio, a unified device
model, ACPI + an AML interpreter, MSI-X interrupts, SMP with per-CPU run-queues,
a concurrent filesystem cache, and transparent fault-driven swap. On the security
side: per-file immutability + an append-only audit log, a TPM 2.0 driver (measured
boot), ChaCha20 full-disk encryption with a TPM-derived key, CoW snapshots with
rollback, a declarative capability-policy engine, an encrypted secrets vault,
kernel crash dumps, and a SMART-based system-health engine. It also carries a
stateful firewall and a sovereign forward-secret VPN (X25519 + ChaCha20-Poly1305,
TPM-seeded); a GNU-compatible coreutils set in the shell (grep, sort, cut, wc,
head/tail, printf, expr, factor, sha256sum, base64, and more); and EuroAgent — AI
agents that run as capability-isolated WASM modules whose trust boundary is the
kernel (not a cloud), with an open MCP gateway and a full audit trail.

New in this build — EuroID, sovereign user management (Sprint K1): a from-scratch
Argon2id credential store (RFC 9106, validated against the official test vector;
64 MB / t=3 / p=4, TPM-seeded salt — never MD5/SHA1/bcrypt), full user & group
model with per-user EuroGuard capabilities, a login flow with timing-attack
prevention and failed-login lockout, and a tamper-evident hash-chain audit log
that records every user action (each entry hashes the previous one, so editing
any past record breaks the whole chain). Try it from the Terminal with the
`eurousers` command. Built to map onto NIS2, GDPR and ISO 27001 access controls.

This is a preview to look at and experiment with — not a daily-driver OS.


WHAT'S IN THIS FOLDER
---------------------
  euroos.img        The bootable EuroOS image (raw, UEFI).
  OVMF.fd           UEFI firmware for QEMU (EDK2/OVMF, BSD-licensed).
  run-euroos.sh     One-click launcher for Linux / macOS.
  run-euroos.bat    One-click launcher for Windows.
  README.txt        This file.


RUN IT — QEMU (recommended)
---------------------------
1. Install QEMU:
     Debian/Ubuntu : sudo apt install qemu-system-x86 qemu-utils
     Fedora        : sudo dnf install qemu-system-x86 qemu-img
     macOS         : brew install qemu
     Windows       : https://www.qemu.org/download/#windows
2. Run the launcher:
     Linux / macOS : ./run-euroos.sh
     Windows       : double-click run-euroos.bat
A QEMU window opens and EuroOS boots. The launcher creates a persistent disk
(euroos-disk.qcow2) on first run — delete it to reset to a clean install.

Tips:
  • Acceleration: Linux uses /dev/kvm, macOS uses HVF, Windows uses WHPX
    (enable "Windows Hypervisor Platform" in Windows Features). Without it the
    OS still runs, just slower (pure software emulation).
  • The Terminal in the desktop is interactive — try:  uname -a · caps · fsck ·
    free · ping gateway · ls / · lsdev · eurosnap · europol · metrics ·
    vault · eurohealth · audit · eurocrash · firewall · vpn · euroagent ·
    eurousers list · eurousers audit --verify-chain ·
    grep · sort · wc · factor 12 · printf · sha256sum · base64


RUN IT — VirtualBox
-------------------
VirtualBox needs the disk-image variant (euroos-preview.vmdk) and EFI enabled:
  1. New VM → Type "Other", Version "Other/Unknown (64-bit)".
  2. Settings → System → Enable EFI (special OSes only).
  3. Settings → Storage → attach euroos-preview.vmdk as the hard disk.
  4. Settings → Network → Adapter 1 → "Paravirtualized Network (virtio-net)".
  5. Start. (VirtualBox EFI is more finicky than QEMU; QEMU is recommended.)


RUN IT — cloud (Whitesky.cloud / OpenStack / any UEFI VM platform)
------------------------------------------------------------------
Use the cloud disk image (euroos-preview.qcow2):
  • Upload it as a custom image / template, set firmware = UEFI.
  • Give the VM a virtio disk + a virtio network interface.
  • Boot. The framebuffer desktop is visible over the platform's console/VNC.
On Whitesky.cloud: create a VM from the uploaded qcow2 with UEFI boot enabled.


REQUIREMENTS
------------
  • An x86-64 host (Intel/AMD). ARM (e.g. Apple Silicon) runs it under emulation,
    which is slower but works.
  • ~1 GB free RAM for the VM.

This preview is provided as-is, for evaluation. EuroOS · sovereign by design.
https://euro-os.eu
