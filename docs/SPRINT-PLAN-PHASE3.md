# EuroOS — Phase 3 Sprint Plan (everything still open)

> **Purpose.** A single reviewable backlog of *all* remaining work, grouped into phases by
> theme and value. Status labels are honest: **✅ done · 🟢 core done (host-tested + boot
> marker), real remainder · 🟡 partial · ⬜ not started · 🔒 needs real hardware (can't be
> fully proven in QEMU/TCG)**.
>
> Items marked **[NEW]** were added in the 2026-06-14 revision (the strategic/compliance pass);
> everything else is unchanged from the first Phase-3 draft.
>
> Two hard rules carry over: **verify by running** (host tests green **and** a `[xx]` boot
> marker) and **never present mock as real** (label demo vs real; under-claim).
>
> **Progress (2026-06-14):** Scheduled **Sprint 1** (3A-1, 3A-2, 3A-7) and **Sprint 2**
> (3A-3, 3C-1, 3C-7, 3C-8, 3C-9) are ✅ **done** — host-tested + boot-verified, and the new
> code was audited ([`CODE-AUDIT-2026-06-14.md`](CODE-AUDIT-2026-06-14.md): 1 CRITICAL + 3
> HIGH + lower fixed with regression tests). 793 host tests. Items are marked ✅ below.
>
> Per-item detail + done-work log: [`NEXT-SPRINTS.md`](../NEXT-SPRINTS.md) (G–Z board).

---

## Where we are (done, boot-verified)

Core OS, EuroFS, storage interop (FAT32 r/w · exFAT/ext r · SMB · NFS · multi-disk + stress),
network/firewall/VPN, EuroDesktop, EuroID, EuroAgent, TPM+FDE+attestation+EuroCA, A/B
self-update, R–Z sovereign-service crates. **793 host tests.**

Recurring pattern: a `🟢 core done` item has a host-tested crate + boot self-test; its
**REMAINING** is the real-world wiring (hardware / live userspace / persistence / interop)
that turns "engine works" into "feature works end-to-end".

---

## Phase 3A — Storage, finished *(no hardware gate, highest practical value)*

| ID | Task | Status | Scope & verification |
|----|------|--------|----------------------|
| 3A-1 | exFAT write | ✅ | create/write/delete/rename in `euroexfat`. Verify: write → `fsck.exfat` clean + Linux reads it. |
| 3A-2 | ext2/3/4 write | ✅ (ext2 w) | block/inode bitmap alloc + extent/indirect write + dir link/unlink. Verify: `e2fsck -f` clean. Start ext2, gate ext4-journal. |
| 3A-3 | Real partition + USB-disk mount harness | ✅ | Mount a *partition* inside a GPT/MBR disk + a removable **USB** volume with **auto-mount** on hotplug. "Plug in a stick and it appears." Verify: extended `run-usb.py` + `[io-usb]`. |
| 3A-4 | NTFS read | ⬜ | `eurontfs` ($MFT, runlists, $DATA). Parked; lower priority. |
| 3A-5 | btrfs / xfs read | ⬜ | Large, lower value. Likely defer. |
| 3A-6 | SMB3 encryption/signing + NFSv4 | ⬜ | Harden the network FS clients vs real Samba/nfsd with signing/encryption required. |
| 3A-7 | **SSD health & durability** **[NEW]** | ✅ | TRIM/discard + write barriers + `fsync` durability guarantees. Underpins data integrity for everything above; cheap, no hardware gate. Verify: discard reaches the device; barrier ordering survives a fault-injection power-cut. |

---

## Phase 3B — Bare-metal readiness *(🔒 protocol cores done, last mile needs metal)*

| ID | Task | Status | The remaining (hardware) part |
|----|------|--------|-------------------------------|
| 3B-1 | Modern virtio transport (unlocks GPU) | 🟢→🔒 | virtio-1.0 transport (PCI caps, common/notify/device cfg) + scanout binding. Verify: `virtio-gpu-pci` + a real scanout screenshot. |
| 3B-2 | WiFi radio driver | 🟢→🔒 | Intel AX200/210 PHY/MAC + iwlwifi firmware + full SAE handshake. Real radio. |
| 3B-3 | Printer live (IPP→CUPS) | 🟢→🔒 | Wire `europrint` to a live TCP connect; verify a real job vs CUPS. + SANE scanning. |
| 3B-4 | ACPI S3 suspend/resume | 🟡→🔒 | CPU + device save/restore across firmware sleep. Not headless-verifiable. |
| 3B-5 | USB hubs + USB EuroFS writeback | 🟡 | Multi-tier hubs; write + mount a USB-resident EuroFS volume. |
| 3B-6 | NVMe MSI-X completion wait-queue | 🟢 | IRQ-blocking wait-queue (vs poll); NVMe MSI-X; live-FS auto-remap. Mostly in-QEMU. |
| 3B-7 | **Audio device driver** **[NEW]** | 🔒 | HDA + USB-audio codec driver (virtio-audio testable in VM first, then metal). Engine half of audio; desktop/routing half is 3F-6. |
| 3B-8 | **Bluetooth** **[NEW]** | 🔒 | HCI stack + pairing; peripherals + BT audio on real hardware. |
| 3B-9 | **Power & thermal** **[NEW]** | 🔒 | CPU freq scaling/governors, ACPI battery, thermal throttling. Laptop usability + eco-argument. |
| 3B-10 | **Multi-monitor / HiDPI / display-hotplug** **[NEW]** | 🔒 | Extends 3B-1 (scanout) into a usable desktop on real panels. |

---

## Phase 3C — Finish the cores *(the 🟢/🟡 REMAINING tails)*

| ID | Task | From | The remainder |
|----|------|------|---------------|
| 3C-1 | Live-FS through the block cache | ✅ | `EuroFs<RootBlk>` → `EuroFs<BlockCache<RootBlk>>` (ripples through `fs`/`fs2`/`vfs`). |
| 3C-2 | Per-subsystem locking | J1 | Per-mount FS / per-conn net / per-CPU run-queue / per-channel IPC; prove no inversions with `eurolock`. |
| 3C-3 | ld.so libc breadth | H3 | Unmodified busybox/curl/sqlite vs a real `libc.so` (symbol versioning + TLS + IRELATIVE). |
| 3C-4 | WASI preview1 complete | H4 | Real `fd_read`/`path_open`/`sock_*`; `wasm <file>` shell command. |
| 3C-5 | Real Wayland clients | H5 | Unmodified libwayland clients over AF_UNIX: fd-passing, `wl_shm`, damage/frame callbacks. |
| 3C-6 | Display-server input/SHM | H2 | Input events back to apps; per-tick live updates; pixel/SHM buffers. |
| 3C-7 | Swap auto-evict under memory pressure | ✅ | Wire CLOCK into the live frame allocator (swap core + fault path already ✅). |
| 3C-8 | Coreutils long-tail + security flags | ✅ | `ln/readlink/realpath/mktemp`, `shuf/comm/join/split`, `sha1/md5/b2sum`, `env`, `xargs`; `--audit`/`--sign`. |
| 3C-9 | FS symlinks | ✅ | Symlink inodes in EuroFS + VFS resolution. Prerequisite for several coreutils. |

> **3C-7 note (reconciled):** J3 swap itself is ✅ done in the G–Z board — only *auto-evict
> under memory pressure* remains, captured here as 3C-7. The older "S5: J3 swap pressure"
> pending task has been **retired** to remove the drift between the task list and the board.

---

## Phase 3D — Sovereign trust, end-to-end *(the European USP)*

| ID | Task | From | The remainder |
|----|------|------|---------------|
| 3D-1 | TPM key sealing → FDE/Vault unseal only on untampered boot | O1+K3 | `TPM2_CreatePrimary/Create/Load/Unseal` + PCR-policy; seal FDE (K3) + Vault master (U) to boot PCRs. **Headline.** |
| 3D-2 | Verity immutable system partition | L3 | Read-only slot mount + Merkle hash tree verified vs the Ed25519 manifest → tamper ⇒ rollback. Needs G4+K3. |
| 3D-3 | Attestation + CA over network | O2+O3 | JSON-over-HTTPS attestation endpoint; real TPM-AK quote; CA intermediates + on-disk store. |
| 3D-4 | Signed policy bundles | X | Ed25519-signed `europol` bundles; per-binary attach; syscall-path cache. |
| 3D-5 | User-scoped immutability + euroattr | L4 | User `IMMUTABLE`/`APPEND_ONLY` on home files; `euroattr` shell cmds; file-manager lock badge. |
| 3D-6 | Vault/GDPR audit persistence | U+P3 | Persist sealed vault + TPM-seal master to PCRs; audit `execve`/connections, JSON + query/export + rotation. |
| 3D-7 | **Secure time (NTS, RFC 8915)** **[NEW]** | ⬜ | Authenticated time sync. Sits *under* all of 3D: attestation freshness, TLS validity and audit-log integrity all need a trustworthy, anti-tamper clock. **Do before relying on 3D-3/3D-6 in the field.** |
| 3D-8 | **Early-boot entropy / RNG quality** **[NEW]** | ⬜ | jitterentropy + hard `getrandom`-blocking guarantees. FDE/EuroCA/attestation/VPN consume randomness early; closes the weakest-link-at-the-worst-moment gap. |
| 3D-9 | **Post-quantum crypto** **[NEW]** | ⬜ | Hybrid ML-KEM in VPN/TLS; ML-DSA / SLH-DSA for EuroCA + update-signing. On-brand future-proofing; cheap differentiator; aligns with EU OSS funding themes. |
| 3D-10 | **eIDAS 2.0 / EUDI-wallet + Belgian eID into EuroID** **[NEW]** | ⬜ | EuroID as a wallet-compatible relying party (W3C Verifiable Credentials, selective disclosure) + Belgian eID middleware (card reader, itsme). Likely the **single strongest differentiator**. **Hard clock: each member state must offer a wallet by Dec 2026.** |

---

## Phase 3E — Distribution & governance *(ship it)*

| ID | Task | From | Scope |
|----|------|------|-------|
| 3E-1 | Installer GUI + FDE enrol | Q1 | Signed userspace installer GUI + live FDE key-enrolment (executor/planner already persist across reboot). |
| 3E-2 | EuroUpdate delivery server | K2 | Fetch signed images over HTTPS → feed G4 A/B apply; stable/beta channels. |
| 3E-3 | Full multi-user / session model | K1 | Multi-user homes, per-user EuroGuard policy, session lifecycle (on EuroID). |
| 3E-4 | Reproducible release pipeline (CI) | Q2 | source-commit → binary-hash → Ed25519 manifest, per-tag (`eurorepro` core done). |
| 3E-5 | EuroToolchain self-hosting | M1 | Native `x86_64-unknown-euroos` Rust std + `eurolibc` + gdb/lldb serial stub. Large. |
| 3E-6 | Package-manager execution | M2 | install/remove/upgrade + content-addressed store (`europkg` resolver done). |
| 3E-7 | OSS governance | Q3 | DCO/CLA, coordinated CVE disclosure (90-day), hardware-compat-list process. Non-code, release-blocking. |
| 3E-8 | **CRA conformance** **[NEW]** | ⬜ | EuroOS is itself a "product with digital elements" → the CRA applies to *us*. Machine-readable **SBOM** in release CI (hooks onto 3E-4), published CVD + vuln-handling process, security updates over the support period. **Dates: vuln/incident reporting from 11 Sep 2026; SBOM + full obligations + CE-marking from 11 Dec 2027.** SBOM must exist *before* Sep 2026 to be able to report at all. |
| 3E-9 | **Disk quota** **[NEW]** | ⬜ | Per-user quota enforcement; couples to 3E-3 multi-user. |

---

## Phase 3F — Apps, desktop & accessibility

| ID | Task | From | Scope |
|----|------|------|-------|
| 3F-1 | EuroContainer runtime | T | OCI-style containers on EuroGuard capabilities (not Linux namespaces) + EuroFS overlay + signed registry + `ResourceLimits`. Needs H4 + S. ⭐⭐ |
| 3F-2 | EuroSuite GUIs | ES-* | Writer/Calc/Impress GUI apps in the compositor + ZIP/deflate container + .xlsx/.pptx/PDF export (compute cores done). Large parallel product track. |
| 3F-3 | **Accessibility (EAA-aligned)** **[NEW — broadened]** | P2 | Was "TTS live"; widen to the full European Accessibility Act surface: screen-reader API, complete keyboard nav, high-contrast, magnification. **The EAA has applied since 28 Jun 2025** — a legal floor, not a nice-to-have. |
| 3F-4 | Locale completeness | P1 | Month names for the remaining 16 EU languages + keyboard layouts (installer keymap). |
| 3F-5 | EuroSuite ↔ OS integration | ES-Int | CoW version history in-UI (= S snapshots), document sandboxing (macros never auto-run), MIME registration, default-app settings. |
| 3F-6 | **Audio routing & desktop integration** **[NEW]** | ⬜ | Mixer, per-app routing, device hotplug, default-device policy. End-to-end half of audio (driver half is 3B-7). Relevant to the conferencing path. |
| 3F-7 | **App permission / portal model** **[NEW]** | ⬜ | Capability-scoped sandbox portals (Flatpak-portals style). Fits "caps, not namespaces" (3F-1); where sovereign data-control becomes visible to the user. |

---

## Phase 3G — Operations, hardening & network completeness **[NEW phase]**

| ID | Task | Status | Scope |
|----|------|--------|-------|
| 3G-1 | Crash-capture + persistent structured journal | ⬜ | Kernel-panic dump (kdump-equivalent) + a journald-style persistent log. 3D-6 covers GDPR audit, but there's no general crash/log story today. |
| 3G-2 | Watchdog timer | ⬜ | Hardware/software watchdog; directly relevant to the OT/industrial pivot. |
| 3G-3 | Kernel-hardening baseline | ⬜ | KASLR, W^X, SMEP/SMAP, stack canaries, microcode load + Spectre-class mitigations. Literal CRA "secure-by-design" evidence (feeds 3E-8). |
| 3G-4 | Security-fuzzing CI + sanitizers | ⬜ | syzkaller-style harness + sanitizer builds in CI; hardening + conformity evidence. |
| 3G-5 | IPv6 completeness | ⬜ | Full dual-stack incl. DHCPv6; finishes the network core listed as done. |
| 3G-6 | mDNS / zeroconf | ⬜ | Service discovery; underpins "plug it in and it appears" (3A-3) + printer discovery (3B-3). |
| 3G-7 | Sovereign DNS resolver | ⬜ | DoT/DoH + DNSSEC validation. Privacy-USP; today there's firewall/VPN but no dedicated DNS layer. |

---

## Suggested order

1. **3A** storage writes + partition/USB harness + SSD durability (3A-7)
2. **3C** quick wins (block-cache live FS, swap auto-evict, symlinks + coreutils)
3. **3D fundamentals first** — NTS (3D-7) + early-boot entropy (3D-8), *then* TPM sealing → FDE/Vault-to-PCR + verity. PQ crypto (3D-9) folds in alongside update-signing.
4. **Compliance-driven 2026 track (parallel, deadline-pressured):**
   - SBOM + CVD in CI (3E-8) — must land before **11 Sep 2026**
   - eIDAS/EUDI into EuroID (3D-10) — target the **Dec 2026** wallet window
   - EAA accessibility (3F-3) — already legally in force
5. **3E** installer GUI + update server + governance + quota
6. **3G** ops & hardening (crash-capture, watchdog, kernel-hardening, fuzzing CI) + network completeness (IPv6/mDNS/DNS) — partly VM-testable, run in parallel
7. **3B** bare-metal batch (modern virtio/GPU first; then audio driver, BT, power/thermal, multi-monitor)
8. **3F** EuroSuite GUIs + audio routing + container runtime + portals (long parallel track)

**Deferred:** hypervisor, distributed storage, native backup, AI runtime, sovereign-cloud.

> **Backup note:** given the GDPR/compliance positioning, a sovereign backup/restore +
> key-escrow story (currently Deferred as "native backup") may need to be pulled forward
> before a real customer deployment — flag for re-scoping rather than leaving fully parked.

---

*Revised 2026-06-14. Companion to `NEXT-SPRINTS.md` (per-item detail + done-work log),
`docs/SPRINT-PLAN-INTEROP.md` (storage), `docs/PHASE2-PLAN.md` (K–Z ordering), and
`docs/EUROSUITE-PLAN.md` / `docs/EUROCOREUTILS-PLAN.md` (product tracks).*
