# EuroOS — Technical Overview

*A from-scratch, sovereign European operating system written in Rust.*

**Document status:** current as of 2026-06-05 · build: `x86-64 UEFI`, alpha · **484 host tests green** (41 library crates), boots to a live desktop, 0 unexpected faults across the verification matrix.

---

## 0. What EuroOS is (and is not)

EuroOS is a **clean-room operating system** built from the boot sector up in Rust. It is **not** a Linux distribution, not a BSD derivative, and not a re-skin of any existing kernel. Every layer — the bootstrap, the memory manager, the scheduler, the filesystem, the TCP/IP stack, the TLS 1.3 client, the X.509 validator, the window compositor — is original code in this repository.

Its identity rests on three pillars:

1. **Sovereignty by construction.** The native surface is **EuroGuard** capabilities, **EuroIPC** message passing, **EuroFS**, **EuroNet**, **EuroTLS**, **EuroUpdate**. There is no dependency on foreign codebases for any security-relevant path. A 25-entry **EU-oriented trust store** anchors TLS.
2. **Linux ABI as a *bridge*, not an identity.** A compatibility layer (`fork`/`execve`/`/proc`-shaped syscalls, ELF loading, ~200 syscall dispatch arms) lets unmodified Linux binaries run, but it is explicitly a *compat shim* on the side of a sovereign core — never the thing that defines the OS.
3. **Memory safety end-to-end.** `#![no_std]` Rust with `#![forbid(unsafe_code)]` on every library crate (the network stack, the filesystem, TLS, the update engine, the sandbox, the display protocol). Unsafe is confined to the kernel's hardware-touching core and audited there.

### Scale

| Component | Lines of Rust |
|-----------|--------------:|
| Kernel (`kernel/src`, 44 modules) | ~15,070 |
| `eurofs` (filesystem) | 2,553 |
| `euronet` (network stack) | 2,400 |
| `eurotls` (TLS 1.3 + X.509 + crypto) | 2,292 |
| `euromm` (memory manager) | 338 |
| `euroupdate` (A/B updates) | 328 |
| `eurodisplay` (display protocol) | 596 |
| `eurosandbox` (containers) | 152 |
| **Total** | **~24,000** |

All of it `no_std`. The seven workspace library crates are **host-testable without a VM** (they compile to `std` under `cargo test`) — 181 unit/integration tests run on the host in milliseconds.

---

## 1. Boot & kernel bring-up

EuroOS is a **single-stage UEFI application**: the kernel *is* the EFI binary (`BOOTX64.EFI`). There is no GRUB, no shim, no second loader (yet — see the roadmap's two-stage A/B work).

**Boot sequence** (`kernel/src/main.rs`, `#[entry] fn main`):

1. **UEFI boot services** — heap init, COM1 serial, frame allocator built from the UEFI memory map (~165 MiB usable in the reference VM).
2. **GOP framebuffer** captured (1920×1080, BGR), ACPI RSDP located from the configuration table.
3. **ExitBootServices** — the OS takes the machine. From here it is entirely self-hosted in ring 0.
4. **GDT + TSS** loaded; the TSS provides separate **IST stacks** for the double-fault handler.
5. **IDT** installed (exceptions, timer, keyboard, mouse, page-fault, GPF, double-fault).
6. **Own paging** — the kernel builds its own 4-level page tables and loads CR3, leaving the UEFI identity map behind.

The serial log narrates every step (`[euro] …`), which is the primary verification channel in headless CI.

### Reference performance

On hardware with virtualization (KVM/HVF/WHPX) EuroOS boots to the desktop in **~1.5–2 s of guest time**. In the sandboxed CI environment there is no `/dev/kvm`, so QEMU falls back to TCG (pure emulation, ~60× slower); the same boot then takes ~145–280 s of wall-clock — a property of the *emulator*, not the kernel.

---

## 2. Memory & paging (`euromm` + `kernel/src/paging.rs`)

- **Frame allocator** (`euromm::FrameAllocator`): region-based physical frame manager seeded from the UEFI map, with contiguous-range allocation for the process pool.
- **4-level paging**, hand-built:
  - **Boot PML4** is supervisor-only.
  - **PML4[0] → PDPT** identity-maps 0–512 GiB with 1 GiB huge pages (covers all RAM + low MMIO).
  - **PML4[1]** maps 512 GiB–1 TiB for high PCI BARs (e.g. NVMe MMIO) and the guarded-stack region.
  - A **shared HIGH_PDPT** is referenced by every process PML4, so kernel-high mappings (MMIO, guarded stacks) are valid under any process CR3.
  - **Per-process PML4s** for `fork`/`execve` isolation; a ring-3 access outside a process's own address space faults and kills *only* that process.
- **Process frame pool** (S3): a 64 MiB contiguous, identity-mapped arena reserved at boot so `fork`/`execve` can allocate page-table frames *while inside a syscall* (when the main allocator is unreachable).
- **W^X / SMEP / SMAP**: user pages are never simultaneously writable and executable; supervisor code cannot be tricked into executing or (under SMAP) reading user pages.

### Guarded kernel stacks (A2 + G1)

Kernel-stack overflow is normally silent corruption. EuroOS places kernel stacks in the shared high region as **uniform guarded units**: each unit is 1 unmapped *guard page* + 4×4 KiB stack pages (16 KiB). A `paging::guarded_stack_alloc()` bump-allocator carves them out of a fine-grained PD→PT (replacing the 1 GiB huge mapping at 512 GiB), and `is_stack_guard(addr)` is an O(1) region+modulo test. An overflow lands on the guard page → **immediate, deterministic `#PF`** instead of clobbering a neighbour.

- **Today:** the mechanism is proven, and all **secondary-CPU (AP) kernel stacks** run on guarded stacks — verified with `-smp 4`: three APs run full per-CPU schedulers, a parallel sum, cross-CPU IPIs, and TLB shootdowns on guarded stacks with zero faults.
- **Next:** scheduler per-task stacks and the IST stacks, which additionally require a dedicated page-fault IST (so the overflow exception's own push doesn't double-fault on the exhausted stack).

---

## 3. Scheduler, processes & SMP (`kernel/src/sched.rs`, `smp.rs`, `ring3.rs`)

- **Preemptive round-robin** scheduler driven by the APIC timer (100 Hz), up to 48 tasks.
- **Process model (S3):** `fork`, `execve`, `waitpid`, zombie reaping, signals, pipes, futexes (`futex_wait`/`futex_wake`). Each process gets its own PML4 and an exclusive kernel stack.
- **Ring 3 / userspace:** a full SYSV64 syscall path (`syscall_dispatch`), ELF loader with **Ed25519 code signing** verified against an in-kernel public key before execution, environment passing, and ~200 Linux syscall dispatch arms.
- **SMP:** application processors are brought up via the Intel **INIT–SIPI–SIPI** protocol from a real-mode trampoline relocated to `0x8000`. Each AP joins `AP_ONLINE`, runs a **per-CPU scheduler with its own run-queue**, participates in cross-CPU **ping-IPIs**, **TLB shootdowns**, and **load-balancing** (the least-loaded core receives migrated tasks).
- **Syscall profiling (D1a):** an RAII HPET-timed wrapper records per-syscall count and nanoseconds (`sprof` shell command) to target hot paths.

---

## 4. EuroFS — filesystem (`eurofs`)

A copy-on-write filesystem designed for integrity-first storage.

- **Copy-on-write** extents; **A/B dual superblock** with generation counters and checksums to survive torn writes.
- **XXH3 checksums** on metadata and data; a **data-path scrubber** detects and (where redundancy allows) repairs corruption.
- **Block device abstraction** (`BlockDevice` trait): any 4 KiB-block device can host EuroFS — implemented for **virtio-blk** and **NVMe**.
- **VFS mount table** (G2): a `Vfs` routes paths to filesystems by **longest-prefix mountpoint** (`/`, `/mnt`, `/nvme`, …). Cross-mount `rename` correctly returns `EXDEV`; `df()` reports per-mount usage. The full 14-method `FileSystem` trait is routed.
- **Operations:** files, directories, create/read/write/rename/rmdir, metadata, persistent across reboot (verified: boot counter increments on a reused disk).
- **On-disk durability:** writes go through a write-back block cache that issues real `VIRTIO_BLK_T_FLUSH` to hardware.

Demonstrated live at boot: three independent EuroFS instances on three devices (two virtio-blk + one NVMe) mounted at `/`, `/mnt`, `/nvme`.

---

## 5. EuroNet — network stack (`euronet`)

A clean-room, RFC-conformant TCP/IP stack, **`#![forbid(unsafe_code)]`**, big-endian-correct via explicit byte conversions.

- **L2:** Ethernet, **ARP**.
- **L3:** IPv4, **IPv6** (link-local + global SLAAC, Router Advertisements), **ICMP** (echo + port-unreachable generation), **ICMPv6 NDP**.
- **L4:** **UDP**, **TCP** with a real state machine, checksums, and **congestion control** (`tcpcc`).
- **Services:** **DHCP** client, **DNS** resolver (with a cache + `/etc/hosts`), HTTP, and **HTTPS over EuroTLS**.
- **Sockets:** a socket table with `Listen`/`Conn`/`Udp` types, `bind`/`listen`/`accept`, and a non-blocking **`poll`/`select` readiness API** (G3) — `sock_poll(fds, deadline)` multiplexes a mixed fd-set (a listener is readable iff its accept-queue is non-empty; a connection iff it has rx data or EOF), with a tick-deadline plus spin-cap so it never blocks forever.
- **Rate limiting** (`ratelimit`) — token buckets guard ICMP error generation.
- **Driver:** virtio-net (PCI), with the parsing/building layers fully host-tested independent of the NIC.

Verified end-to-end at boot: DHCP lease → ARP → ICMP ping → DNS → `HTTP GET` → `HTTPS GET` (encrypted) → IPv6 SLAAC, plus POSIX-socket HTTP/DNS through the Linux-ABI layer.

### Unix-domain sockets — AF_UNIX (H1)

`euronet::unix::Switchboard` is a kernel-wide **local socket switchboard**: a single owner of all listeners and connections (no shared ownership / Rc), addressable by path. It provides `bind_listen`/`connect`/`accept`/`send`/`recv`/`readable`/`close` with POSIX-shaped errors (`ECONNREFUSED`, `EADDRINUSE`, backlog, `EPIPE`, EOF), bidirectional byte FIFOs, and a lightweight copyable `Endpoint` handle. The kernel exposes `net::unix_*`; verified by a boot-time ping/pong round-trip including EOF-after-close, plus 8 host tests. This is the substrate the live display server runs on.

---

## 6. EuroTLS — TLS 1.3, X.509 & crypto (`eurotls`)

A sans-IO **TLS 1.3 client** (RFC 8446) and the cryptographic primitives beneath it — own implementations, host-tested.

- **Handshake:** TLS 1.3 state machine, `TLS_CHACHA20_POLY1305_SHA256`, **X25519** key exchange, HKDF key schedule, AEAD record layer.
- **Certificate validation (A1):** a from-scratch **DER/X.509** parser and **chain validation** against a **25-root EU-oriented trust store** baked into the kernel.
- **Signature algorithms:** **Ed25519**, **ECDSA P-256/P-384**, **RSA PKCS#1 v1.5** and **RSA-PSS** with SHA-256/384.
- **Usage:** drives `https` in the shell and the network self-test; the same signature verification gates **code signing** (ELF binaries) and **update images** (EuroUpdate).

---

## 7. EuroGuard — security model

Capabilities, not ambient authority. EuroGuard is the native authorization surface:

- **Capability-scoped syscalls:** sensitive syscalls require a capability (e.g. `CAP_NET` for network access, `CAP_FILE` for filesystem, `CAP_CONSOLE`). Capabilities can be **dropped but never regained** within a process.
- **Memory isolation:** per-process address spaces; an out-of-bounds access kills only the offending process (the desktop and other processes keep running) — demonstrated by the isolation page-fault path.
- **Code authenticity:** binaries carry an **Ed25519 signature** over their bytes, verified against the in-kernel public key before they are allowed to run.
- **W^X, SMEP, SMAP** enforced in hardware.
- **Auditing:** EuroIPC and capability decisions are logged.

### EuroSandbox — containers (`eurosandbox`)

`Container` provides capability-scoped, **chroot-safe** sandboxes: a container has an effective capability set that can only shrink, and path resolution is escape-proof (`../../../etc/passwd` resolves *inside* the container root). Demonstrated at boot: a `demo` container with `CAP_NET` revoked and a contained path-traversal attempt.

### EuroID — user management, credentials & audit (`euroid`, Sprint K1)

The identity authority that binds users to the capability model. Host-tested core (`crates/euroid`, 24 tests) + the kernel `euroid` module, surfaced as the `eurousers` shell command.

- **Sovereign Argon2id** — Blake2b (RFC 7693) + Argon2id (RFC 9106) implemented from scratch, validated against the **official RFC 9106 test vector**. Defaults 64 MiB / t=3 / p=4 with a 32-byte TPM-RNG salt; passwords are never MD5/SHA1/bcrypt and the cost is never negotiated down.
- **User & group model** — `UserId`/`GroupId` newtypes (never a raw `u32`), `User`/`UserState` (Active/Locked/Expired/Deleted — deleted records are *kept* for audit), six built-in groups (wheel/audit/net/vault/agent/users), and password history (no reuse of the last 12).
- **EuroAuth login flow** — `authenticate()` with **timing-attack prevention** (an unknown user runs an identical dummy Argon2id verify, so unknown-user and wrong-password are indistinguishable in time *and* error), failed-login lockout (5 attempts), `must_change`, and per-session capability derivation: `effective_caps` = the user's own caps ∪ their groups' caps, then bounded by the EuroPol allow-mask — **policy can only reduce, never add**.
- **Tamper-evident hash-chain audit log** (P3) — every user action is serialised as a self-describing JSON record whose hash covers `seq ‖ prev_hash ‖ body`. Editing any past entry invalidates every later hash; `eurousers audit --verify-chain` reports integrity and the root hash. UIDs (not usernames) are the key, for GDPR Art. 32 pseudonymisation.
- Verified end-to-end at boot by the `[k1]` self-test (useradd → login → unknown-user → 5×-lockout → soft-delete → chain verify) which also live-exercises the `eurousers` shell path. Maps onto NIS2 Art. 21, GDPR Art. 5(2)/32, and ISO 27001 A.9.

---

## 8. Display & desktop (`eurodisplay`, `kernel/src/compositor.rs`, `dispserv.rs`)

- **EuroDisplay protocol** (`eurodisplay`): a Wayland-shaped surface protocol — `Request` (CreateSurface / Attach / Commit / Move / Destroy), `Event` (Configure / Key / Pointer / FrameDone), a z-ordered surface model with damage tracking, and a 12-byte wire encoding. Pure, host-tested protocol logic.
- **EuroDesktop compositor:** a real framebuffer compositor with windows (title bar, traffic-light controls, "Protected" security pill, shadow, body content), a dock/sidebar, a live status panel (clock, RAM bar, uptime, cores, process count), and a mouse cursor with save-under. Dirty-rectangle updates keep per-frame cost low.
- **Live display server (H2):** `eurodisplay::server` carries `Request`s plus compositor metadata (window title, content lines) over a byte stream as length-prefixed frames, and `ServerView` translates mapped surfaces into draw-ready `WindowView`s. The kernel `dispserv::DispServer` **binds an AF_UNIX socket** (`/run/eurodisplay.sock`), accepts clients, decodes frames, and emits **real compositor windows**. Verified: an in-kernel app connects over AF_UNIX (H1) and opens a genuine third desktop window — it exists *because another process asked for it over a socket*, not because it was hard-coded.
- **Theme:** the EuroDesktop System (EDS) light theme with original `euicon`/`appicon` iconography.

---

## 9. Storage drivers & partitioning

- **virtio-blk** (`virtio_blk.rs`): multi-disk, with a write-back block cache and real flush-to-hardware.
- **NVMe 1.4** (`nvme.rs`): PCI discovery, admin + I/O queues, PRP lists, SMART (temperature, wear), completion polling, and a `BlockDevice` wrapper so **EuroFS runs on NVMe**.
- **GPT** (`gpt.rs`): protective MBR + GPT header + 128-entry partition array; reads any EuroFS partition and installs the on-disk layout.

---

## 10. EuroUpdate — atomic A/B system updates (`euroupdate`)

An anti-brick A/B slot state machine (host-tested, 5 tests): slots A/B with states (Empty / Trying / Good / Failed), a boot-attempt counter, generation counter, `stage_update` / `on_boot` / `mark_good` / `rollback`, and a 32-byte checksummed (Fletcher-32) serialization.

- **G4 durability:** the `slot_config` now lives on a **raw GPT-reserved block** (LBA 40, in the alignment gap before the first partition at LBA 2048 — *outside any EuroFS partition*). It is read/written via direct `virtio_blk` sector I/O with flush, so the A/B state **survives filesystem corruption and torn writes** (the top reliability risk). The FS file `/boot/slot_config` is kept only as a human-readable mirror.
- **Verified across a real reboot:** boot 1 on a fresh disk reports *"fresh disk → initial"*; boot 2 on the same disk reports *"recovered from previous boot"* — proving FS-independent persistence.
- **Signed updates:** `euroupdate apply` verifies an **Ed25519 signature** over the image before staging it to the inactive slot.

---

## 11. Linux ABI compatibility (EuroCompat)

A deliberate *bridge* for running unmodified Linux binaries:

- **ELF loader** with code-signature verification; ~200 syscall dispatch arms (`read`/`write`/`open`/`close`/`mmap`/`brk`/`fork`/`execve`/`wait4`/`getpid`/`uname`/`pipe`/`dup`/`futex`/`arch_prctl`/`nanosleep`/…).
- **EuroIPC bridge syscalls** (500–502) for native message passing alongside the POSIX surface.
- Demonstrated: an unmodified C program runs in ring 3 and performs HTTP `GET` and DNS lookups via POSIX sockets on top of EuroNet.

This layer is explicitly compat scaffolding — the native EuroGuard/EuroIPC surface is the identity.

---

## 12. Platform services

- **ACPI** parsing (RSDP/RSDT/MADT) for CPU topology and power.
- **APIC** (local + IO-APIC) for SMP and interrupt routing; **HPET** as the high-resolution time source (used for syscall profiling and uptime).
- **PS/2** keyboard + mouse; **RTC** wall-clock; **PCI** enumeration.
- **EuroInit:** a service supervisor (ticker tasks with restart policy) and **observability** — a leveled kernel log ring (`dmesg`), 512-line kmsg buffer.
- **Interactive shell** with ~45 commands: `help uname free ps lspci df mem ls cat write mkdir rm rename rmdir net netstat resolve ping ping6 wget https tcpserve euroguard scrub euroctl dmesg ctr/create/run (containers) eup/status/rollback/apply (updates) sprof uptime su id …`.

---

## 12b. Phase 2 — hardware breadth & the sovereign-platform layer (new)

Beyond the core OS above, the build now carries a full hardware + sovereignty layer, each subsystem host-tested where it's pure logic and **boot-verified** in QEMU.

**Hardware & kernel:**
- **USB (`eurousb` + `kernel/src/xhci.rs`)** — a real **xHCI** USB-3 controller driver: controller reset, DCBAA + scratchpads, command/event TRB rings, port reset, full enumeration (Enable-Slot → Address-Device → GET_DESCRIPTOR → Configure-Endpoint), **interrupt-IN HID** (keyboard/mouse → the PS/2 scancode + mouse paths) **and Bulk-Only-Transport mass storage** (SCSI INQUIRY/READ-CAPACITY/READ-10). Input is harvested in the **MSI-X IRQ handler** so it works under HLT-idle.
- **Audio (`euroaudio` + `kernel/src/hda.rs`)** — an **Intel HD-Audio** driver: CORB/RIRB codec enumeration, output-path routing, stream-DMA playing a mixed tone (LPIB advances = real playback).
- **Device model (`eurodevice`)** — a unified `DeviceTree` + `DriverRegistry` built from PCI enumeration; existing drivers (virtio/NVMe/xHCI/HDA) bind through it. `lsdev` shows the tree.
- **ACPI + AML (`euroacpi`, `euroaml`)** — table parsing **plus a minimal AML bytecode interpreter** that walks QEMU's real DSDT, evaluates `\_S5` (drives ACPI shutdown), and finds `_STA`/thermal/battery methods.
- **MSI-X (`kernel/src/msix.rs`)** — PCI-capability MSI-X programming → LAPIC message delivery; interrupt-driven completion on the **USB input** and **virtio-blk storage** paths.
- **Transparent swap (`euromm::swap` + `kernel/src/swapmgr.rs`)** — a page faults on a not-present PTE (swap-slot encoded in the upper bits) → the page-fault handler reads it back from disk and resumes. SMP with **per-CPU run-queues**; a **concurrent (RwLock) FS block cache** (read-lock hits scale across cores); **HLT-idle**.

**Sovereignty & security spine:**
- **Immutability (`eurofs` L1/L2)** — per-inode `IMMUTABLE`/`APPEND_ONLY` flags enforced in the FS (write/delete/rename rejected even from ring-0); setting/clearing them requires the new `CAP_IMMUTABLE_ADMIN` capability — tamper-proof system files even for root.
- **Audit log (`kernel/src/audit.rs`)** — an **append-only** `/var/log/audit.log` (L1) of security events; truncation/overwrite rejected by the FS, grows monotonically across reboots (tamper-evident).
- **TPM 2.0 (`eurotpm` + `kernel/src/tpm.rs`)** — a TIS-MMIO driver: Startup, GetRandom, **PCR read/extend** (measured boot). Verified against QEMU `tpm-tis` + swtpm.
- **Full-disk encryption (`eurofde`)** — length-preserving per-block **ChaCha20** FDE (`EncryptedBlockDevice` is a transparent `BlockDevice`), keyed by a **TPM-generated 256-bit key**.
- **Snapshots (`eurofs` EuroSnap)** — CoW snapshots = frozen root-pointers pinned in the allocator; `snapshot_create/rollback/delete` + GC; `eurosnap` command. Rollback on the live root FS keeps it scrub-clean.
- **Policy engine (`europol`)** — declarative `[allow]/[deny]` policy → an EuroGuard capability mask (`(base|allow) & !deny`, deny wins) + path rules; violations → the audit log. `europol`/`europol explain`.
- **Secrets vault (`eurovault`)** — capability-gated secrets, `EPERM` without the right `read_caps` (even root), volatile-zeroed on drop, sealed/unsealed with ChaCha20-Poly1305 (tamper-evident) under the TPM master key. `vault`.
- **Observability (`euroobserve`)** — lock-free `Counter`/`Gauge`/`Histogram` + an **OpenMetrics** (Prometheus) renderer. `metrics`.
- **Crash recovery (`eurocrash`)** — a 512-byte minidump (registers/cr2/cr3/vector) written to a reserved block on `#GP`/`#PF`/`#DF`, read back on the next boot (recovery). `eurocrash`.
- **Health engine (`eurohealth`)** — parses the NVMe **SMART** log + FS scrub + memory into a 0–100 health score. `eurohealth`.

**Networking, coreutils & the agent-first runtime (Phase 2B):**
- **Firewall (`eurofw` + `kernel/src/firewall.rs`)** — a stateful packet-filter: 5-tuple rules, a connection-tracking table, default-deny ingress. `firewall`/`eurofw`.
- **VPN (`eurovpn` + `kernel/src/vpn.rs`)** — a sovereign, **forward-secret** tunnel: a Noise-style **quadruple X25519-DH** handshake → HKDF-SHA256 → **ChaCha20-Poly1305** transport, identity seeded from the **TPM** RNG. Boot proves a full initiator/responder handshake + an encrypted mutual round-trip. `vpn`/`eurovpn`.
- **Coreutils (`eurocoreutils`)** — a GNU-compatible coreutils core, each command a pure `fn(args, input) -> Vec<u8>` host-tested against expected GNU output, wired into the shell (the last file argument is read as stdin): `echo · seq · basename · dirname · head · tail · wc · tac · rev · nl · fold · cat -n · sort · uniq · cut · tr · grep` + checksums/encoding `sha256/512/224/384sum · base64 · base32 · cksum`, plus FS ops `cp · touch · stat · truncate` (where `stat` shows the L1 immutability flags).
- **EuroAgent (`euroagent` + `kernel/src/agent.rs`)** — the sovereign agent-first runtime, the EU answer to Microsoft's **Project Solara**. Agents are WASM modules with a **declarative capability manifest** (TOML); the trust boundary is **in the kernel** (EuroGuard), not a US cloud. The effective capability set is a strict subset — `(required | granted_optional) ∩ user_caps − EuroPol_denied` — so an agent never gets more than the parent user, and elevated caps force user confirmation. A native **MCP gateway** (JSON-RPC 2.0, 10 tools each gated on its `CAP_AGENT_*`) exposes EuroFS/EuroNet/EuroVault/EuroDisplay; **every call is audited** (P3 shape). Deterministic **intent routing** (no AI in the dispatcher) maps a spoken/typed intent to an agent. A local LLM is the default (an Ollama-compatible `LlmBackend`); cloud is opt-in via EuroVault. The **agent execution loop** drives `model → tool → MCP-gate → result → model → final answer`, every tool call cap-gated and audited. Agents ship as **Ed25519-signed `.euroa` bundles** (a domain-separated signature over `manifest || wasm`), so a tampered manifest or WASM is refused — the chain *publisher → bundle → running agent* is cryptographically sealed. Boot proves the whole chain end-to-end: manifest → least-privilege caps (NET dropped by policy, EXEC by user-clamp) → cap-gated MCP call (fs_write allowed, exec denied) → intent route → LLM↔MCP loop → signed-bundle verify (valid accepted, tampered rejected). Shell subcommands: `euroagent caps · mcp list · inspect · dispatch test <intent>`. `euroagent`/`agent`.

- **EuroLocale (`eurolocale` + `kernel/src/locale.rs`)** — sovereign localisation for **all 24 official EU languages** (a European OS must speak every EU language, not just English). A CLDR-style core, host-tested, no external data blobs: locale-aware **number** formatting (per-language grouping + decimal separators), **currency** (€ for the eurozone or the national currency — BGN/CZK/DKK/HUF/PLN/RON/SEK — with correct symbol placement), **date** patterns (DMY vs ISO YMD + month names), **plural** rules (the full CLDR systems: one/two/few/many/other across the Slavic, Baltic, Celtic and Romance families), and tailored **collation** (diacritic folding so `é` sorts by `e`, plus per-language tailoring: Swedish `å/ä/ö` after `z`, German `ä≈a`, Spanish `ñ` after `n`, Czech `č` after `c`). Boot proves currency/date/plural/collation across nl/en/de/sv/pl/fr. `locale`/`locale <tag>`.
- **EuroAgent real tools & EuroInstall** — the MCP gateway's `FsToolBackend` now wires `fs_read`/`fs_write`/`display_notify` to **real EuroFS**, each agent clamped to its `/agents/<name>/` sandbox (path-traversal stripped); boot proves an agent really reads/writes on disk, that an escape stays in the sandbox, and that a missing cap is denied. The LLM path builds real **Ollama HTTP/1.1** requests (`ollama_http_request`) and parses responses. **EuroInstall (`euroinstall`)** is the guided-installer/live-image **planner**: host-tested GPT layout (ESP + EuroOS-A/B + EuroVar), config validation, and an ordered step plan (FDE-before-format, user, EuroCA, A/B finalize) with a disk-free **live mode**. `euroagent llm`, `euroinstall [live]`.

### Shell command reference (all working today)

The interactive terminal supports pipes (`a | b`), redirection (`>`/`>>`), arguments, and running signed programs by name. Built-ins, by category:

| Category | Commands |
|----------|----------|
| Filesystem | `ls` · `cat` · `write` · `mkdir` · `rm` · `rmdir` · `mv`/`rename` · `df` · `fsck`/`scrub` · `fsck repair` |
| System & processes | `uname -a` · `hostname` · `free` · `mem` · `date` · `uptime` · `ps` · `kill` · `dmesg` · `lspci` · `reboot` · `shutdown`/`poweroff` · `clear` · `help` |
| Users & session (EuroID/K1) | `login` · `su` · `sudo` · `logout` · `whoami` · `id` · `eurousers list`/`show`/`add`/`passwd`/`lock`/`unlock`/`del`/`groups` · `eurousers audit --verify-chain` |
| Network | `net` · `netstat` · `ping` · `ping6` · `nslookup`/`resolve` · `fetch`/`wget` · `https` · `tcpserve` · `firewall`/`eurofw` · `vpn`/`eurovpn` |
| Coreutils (GNU-compatible) | `echo` · `seq` · `basename` · `dirname` · `head` · `tail` · `wc` · `tac` · `rev` · `nl` · `fold` · `sort` · `uniq` · `cut` · `tr` · `grep` · `find` · `sha256sum`/`sha512sum` · `base64` · `base32` · `cksum` · `cp` · `touch` · `stat` · `truncate` |
| Security & sovereignty | `caps`/`euroguard` · `europol` · `vault` · `audit` · `eurosnap` · `eurocrash` · `eurohealth` · `lsdev`/`eurodevice` · `metrics` |
| Agents | `euroagent`/`agent` (manifest, capability-isolation, MCP gateway, intent dispatch) |
| Localisation | `locale` / `locale <tag>` (24 EU languages: number/currency/date/plural/collation) |
| Services, containers & updates | `services`/`euroctl` · `container`/`ctr` · `euroupdate`/`eup` · `install <pkg>` · `sprof` |

---

## 13. Test & verification posture

- **690 host tests** (no VM, run under `std`) across **57 library crates**: eurofs · euronet · eurotls · euromm · euroupdate · eurosandbox · eurodisplay · eurowasm · eurowl · euroaudio · europrint · eurousb · euroacpi · euroaml · eurodevice · eurotpm · eurofde · europol · euroobserve · eurovault · eurocrash · eurohealth · eurofw · eurovpn · eurocoreutils · euroagent · eurolocale · euroinstall · euroca · euroattest · euroidm · **euroid** (Sprint K1: from-scratch Argon2id verified against the RFC 9106 test vector) · europkg · eurorepro · euroaccess · the EuroSuite/office + EuroApps crates · eurojs · euroweb. Clippy-clean.
- **Tier-2 verification:** UEFI build → QEMU boot → serial-log assertions → framebuffer screenshot. Every feature in this document was confirmed to boot to the desktop with **0 unexpected faults**.
- **Multi-config harnesses:** single-disk, two-virtio-blk + NVMe multi-disk, and `-smp 4` SMP boots.
- *Note:* the kernel binary itself cannot be host-unit-tested (it is `no_std` with its own panic handler — `cargo test --workspace` would hit a duplicate `panic_impl`); kernel paths are verified by boot self-tests. Use `cargo test -p <crate>` per library crate.

### How to build & run

```bash
# Build the UEFI binary + bootable FAT32 image
./scripts/build.sh release        # → eurokernel.img

# Boot in QEMU (headless, serial + screenshot)
python3 scripts/screenshot.py eurokernel.img boot.png

# Multi-disk + NVMe + SMP harness
SMP=4 python3 scripts/run-multidisk.py eurokernel.img out.png 200

# Write to USB and boot any UEFI x86-64 machine
sudo dd if=eurokernel.img of=/dev/sdX bs=4M status=progress   # check lsblk first!
```

The canonical build is **release** (`cargo kbuild-release`); the debug profile currently fails on SIMD codegen and is not used.

---

## 14. Recently completed (this development cycle)

Sprints G–J fully landed (guarded stacks, A/B durability, dynamic linker, WASM/WASI, Wayland, USB/audio breadth, concurrency). The current cycle added the **Phase-2 sovereign-platform layer** (§12b), all boot-verified:

| Area | Delivered |
|------|-----------|
| **Hardware** | xHCI USB (HID + mass storage) · Intel HD-Audio · `eurodevice` device model · ACPI AML interpreter · MSI-X (USB + storage) · transparent fault-driven swap |
| **Reliability** | concurrent RwLock FS cache · CoW snapshots + rollback (EuroSnap) · kernel crash dumps (EuroCrash) · SMART health engine (EuroHealth) · lock-free kmsg + OpenMetrics (EuroObserve) |
| **Sovereignty** | file immutability + `CAP_IMMUTABLE_ADMIN` · append-only audit log · TPM 2.0 (measured boot) · ChaCha20 FDE (TPM-keyed) · capability-policy engine (EuroPol) · encrypted secrets vault (EuroVault) |
| **Identity (K1)** | EuroID user management — from-scratch Argon2id credentials (RFC 9106-verified) · per-user EuroGuard capabilities · timing-attack-safe login + failed-login lockout · tamper-evident hash-chain audit log · `eurousers` CLI (NIS2 / GDPR / ISO 27001) |

See **`ROADMAP.md`** + **`docs/PHASE2-PLAN.md`** for what's planned next and **`NEXT-SPRINTS.md`** for the per-task sprint board.

---

*Live site: <https://euro-os.eu> · docs: <https://euro-os.eu/docs/> · try it: <https://euro-os.eu/try/> · whitepaper: <https://euro-os.eu/whitepaper/>*
