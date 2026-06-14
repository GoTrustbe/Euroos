# EuroOS

**A sovereign, security-first operating system for Europe — built from scratch in Rust.**

EuroOS is its own kernel, filesystem, network stack, capability security model and
desktop — `no_std`, x86-64 UEFI, with **no Linux or BSD underneath**. Sovereignty
here is an *architectural* property, not a label: it is baked into every syscall,
every binary signature check, and every network call. Zero telemetry. Licensed
under the **European Union Public Licence (EUPL) v1.2**.

> ⚠️ **Alpha preview.** EuroOS boots to a working desktop with real networking and
> on-disk persistence, and **755 host tests** pass. It is something to study,
> build on, and experiment with — not yet a daily-driver OS.

**Try it without building:** one-click QEMU / VirtualBox / cloud images at
**[euro-os.eu/try](https://euro-os.eu/try/)**. With hardware virtualization it
boots in ~1–2 seconds.

---

## What works today (all boot-verified)

**Core OS**
- Two-stage UEFI boot → `ExitBootServices` → its own kernel mode (own GDT/IDT,
  4-level paging, heap, exceptions, COM1 debug).
- Preemptive multitasking, per-CPU run-queues, SMP, HLT-idle.
- **Ring-3 userspace + `SYSCALL`** with enforced privilege separation and
  centrally-validated user pointers at the syscall boundary.
- A Linux/musl ABI **compatibility bridge** (run unmodified static binaries) —
  a bridge, never the identity.
- **EuroFS** — a copy-on-write on-disk filesystem (inodes, extents, checkpoints,
  CoW snapshots + rollback, crash-consistent A/B superblocks, data checksums).
- **Mounts other filesystems too** — FAT32 (read **+ write**), exFAT and ext2/3/4 (read),
  plus **SMB2/3** (NTLMv2) and **NFSv3** network shares — each verified against the real
  reference tools (`fsck.fat`/`mtools`, `mkfs.exfat`, `mkfs.ext4`, Samba, Linux `nfsd`).
  A `format`/mkfs command and an auto-detecting `mount`/`lsblk`; multi-disk tested to 64 GiB.
- A real **network stack** (Ethernet/ARP/IPv4/IPv6/ICMP/UDP/TCP/DNS/DHCP), a
  stateful **firewall**, and a forward-secret **VPN** (X25519 + ChaCha20-Poly1305).
- **EuroDesktop** — a windowed compositor (z-order, dragging, PS/2 + USB input),
  an interactive shell, a GNU-compatible **coreutils** set, and a live display server.

**Sovereign identity & Zero-Trust security**
- **EuroID** — sovereign user management with memory-hard **Argon2id** credentials
  (RFC 9106), account lockout, and a tamper-evident **hash-chain audit log**. The
  user store, password hashes and audit **persist across reboots**.
- A **GUI lockscreen** authenticates the desktop session against EuroID
  (Argon2id, not a Linux PAM stack), with enforced password changes.
- **EuroAgent** — capability-isolated AI agents (the sovereign answer to cloud
  agent runtimes): agents declare a capability manifest, are isolated **at the
  kernel**, and every tool call goes through an audited MCP gateway. Zero-Trust
  controls: **just-in-time capability elevation** (per-action, auto-revoked),
  deterministic **behavioral anomaly detection**, and a **persistent** audit trail.
- Hardware root of trust: a **TPM 2.0** driver with **measured boot** (PCR
  extend), **ChaCha20 full-disk encryption**, and **PCR-sealed secrets** — the
  vault and disk keys unseal only on an untampered boot state.
- Per-file **immutability** + an append-only audit log; **fail-closed TLS 1.3**
  (a missing trust anchor refuses the connection); from-scratch, constant-time crypto.

Every binary is **Ed25519-signed and verified before it runs**. See
[`STATUS.md`](STATUS.md) for the full per-subsystem status and the roadmap.

## Try / build / test

```bash
# Requirements: Rust nightly is pinned by rust-toolchain.toml; plus QEMU + tools:
sudo apt install qemu-system-x86 ovmf dosfstools mtools   # Debian/Ubuntu

./scripts/test.sh          # the canonical gate: host tests + clippy + kernel build + QEMU boot
./scripts/build.sh release # build the bootable eurokernel.img
./scripts/run-qemu.sh      # boot it (GUI, or headless without $DISPLAY)
```

The security-critical logic lives in pure, host-tested `crates/euro*` libraries
(`cargo test`, no VM); the kernel module is thin glue that wires it onto hardware
and prints a `[xx]` boot self-test. That is why there are 750+ host tests **and** a
live boot marker for almost everything.

## Repository layout

```
eurokernel/
├── kernel/            # the no_std UEFI kernel (main, paging, sched, ring3, compositor, drivers …)
├── crates/euro*/      # ~50 host-tested libraries (eurofs, euroid, euroagent, eurotls, euronet, eurovault …)
├── userland/          # C sources → signed ring-3 programs
├── toolchain/         # eupkg package manager + signing tooling (public key only)
├── scripts/           # build.sh, run-qemu.sh, screenshot.py, test.sh
├── docs/              # deep technical reference, roadmap, security audit, Zero-Trust mapping
└── .github/workflows/ # CI: host tests + clippy + kernel build + headless QEMU boot
```

## Documentation

- [`STATUS.md`](STATUS.md) — what's built and how it works, with the roadmap.
- [`docs/EUROOS-DEEP-TECHNICAL-REFERENCE.md`](docs/EUROOS-DEEP-TECHNICAL-REFERENCE.md) — the deepest per-subsystem reference.
- [`docs/CODE-AUDIT-2026-06-10.md`](docs/CODE-AUDIT-2026-06-10.md) — the full internal security audit.
- [`docs/ZERO-TRUST-FOR-AI-AGENTS-MAPPING.md`](docs/ZERO-TRUST-FOR-AI-AGENTS-MAPPING.md) — how EuroAgent maps onto Zero-Trust.

## Contributing

Contributions are welcome from across Europe and beyond. Please read
[`CONTRIBUTING.md`](CONTRIBUTING.md) — we use the lightweight **Developer
Certificate of Origin** (`git commit -s`, no CLA), and hold two habits sacred:
**verify by running** (tests green + a boot marker), and **never present mock as
real**. Be kind: see the [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

## Security

Found a vulnerability? Please email **jeroen@gotrust.be** privately — do not open a
public issue. See [`SECURITY.md`](SECURITY.md).

## License

Copyright (c) 2026 EuroOS Contributors / GoTrust.

EuroOS is licensed under the **European Union Public Licence (EUPL) v1.2** — see
[`LICENSE`](LICENSE). Third-party component licences are listed in [`NOTICE`](NOTICE).
The EUPL is OSI-approved, copyleft, and explicitly compatible with both the
permissive dependencies EuroOS builds on and the major copyleft licences — a
natural fit for a sovereign European project.
