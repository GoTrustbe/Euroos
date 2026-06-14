# EuroOS — Status & Roadmap

*A sovereign, security-first operating system for Europe — built from scratch in Rust (`no_std`, x86-64 UEFI). No Linux or BSD underneath. Sovereignty is an architectural property, baked into every syscall, every binary verification, and every network call.*

Last updated: 2026-06-14 · **755 host tests green** · 55+ host-tested library crates · boots to desktop in ~1.5–2 s (on KVM/HVF/WHPX).

> **Phase 2F — storage interoperability + reliability + an all-English codebase (newest, all verified):** EuroOS now mounts the world's filesystems alongside its own **EuroFS**. New mountable drivers, each verified against the real reference tools: **FAT32** read **+ write** (`eurofatfs`; cross-checked with `fsck.fat`/`mtools`), **exFAT** read (`euroexfat`; vs `mkfs.exfat`), **ext2/3/4** read (`euroext`; vs `mkfs.ext4`, incl. ext4 extent trees), an **SMB2/3 client** with **NTLMv2** (`eurosmb`; vs a real Samba server), and an **NFSv3 client** (`euronfs`; vs a real Linux `nfsd`) — the last two boot-verified end-to-end over the live NIC. A standalone **`format`/mkfs** command (FAT32 or EuroFS) and an auto-detecting **`mount`/`umount`/`lsblk`** framework. Multi-disk support was **load- and functional-tested 8 MiB → 64 GiB** (format → fill → verify → delete → reformat + cross-disk copy), surfacing and fixing a real multi-cluster-directory data-loss bug and an O(n²) allocator. A separate **big load/stress test** (`stresstest` + `run-stresstest.py`) then ran the OS with its root on a real on-disk EuroFS partition through sustained external-disk write/rename/delete/rewrite churn, a cross-disk move, **filling the on-disk root filesystem until full** (the real "boot disk is full" case — existing data survives and the FS recovers), and **multiple concurrent programs** — all phases pass with no frame leak and a clean scrub. Reliability hardening (**Sprint 5**): a fault-injection proof of EuroFS crash-consistency (power-cut + torn-write at every write point), a lock-order checker (`eurolock`), and block-under-load tests. The **entire codebase was translated to English** (comments + runtime strings; the Dutch-locale screen-reader strings intentionally kept). Earlier forward-plan items also landed: TLS dynamic linking, clean ACPI shutdown, and **secure A/B self-update** — the loader owns the boot-attempt counter and rolls back on its own, with **Ed25519 verify-before-activate** (a tampered image is refused). Plans: [`docs/SPRINT-PLAN-INTEROP.md`](docs/SPRINT-PLAN-INTEROP.md) + [`docs/SPRINT-PLAN-FORWARD.md`](docs/SPRINT-PLAN-FORWARD.md).

> **Phase 2E — "Promise = Reality" breadth: desktop apps, web media/forms, a sovereign installer & coreutils long-tail (newest, all boot-verified):** three real **desktop apps** open from the dock with live data — **EuroFiles** (the actual EuroFS), **EuroNotes** (the real `euronotes` Markdown engine), **EuroClock** (real RTC + EU world clocks); the dock was rebuilt to honest per-app tiles. The **EuroWeb** browser now renders real **images** (QOI **+** a from-scratch PPM decoder via `euromedia`) and interactive **HTML forms** (`<input>`/`<button>` with a real `GET` submit). A **sovereign installer**: a new from-scratch **FAT32** writer (`eurofat`, with long-filename support) assembles a GPT + EFI System Partition + EuroFS root; the kernel reads its **own** loader+kernel off its boot ESP via UEFI (no embedded copy) and writes a genuinely **bootable** disk to a blank target — which then boots EuroOS **standalone** (cross-validated by `fsck.fat`, `mtools`, `sgdisk`, and a real QEMU boot). Plus the **coreutils** long-tail: `xargs` (+ `-n N`) as a pipeline stage and more pipe-stdin filters. This completes the "Promise = Reality" cycle ([`docs/SPRINT-PLAN-AD-AG.md`](docs/SPRINT-PLAN-AD-AG.md)) and the install/update/WASM cycle ([`docs/SPRINT-PLAN-AH.md`](docs/SPRINT-PLAN-AH.md), AH-1/2/3 ✅); the **forward plan** for what's left is [`docs/SPRINT-PLAN-FORWARD.md`](docs/SPRINT-PLAN-FORWARD.md).

> **Phase 2D — full code audit + Zero-Trust depth + EuroID end-to-end + GUI lockscreen (all boot-verified):** a full security/correctness audit ([`docs/CODE-AUDIT-2026-06-10.md`](docs/CODE-AUDIT-2026-06-10.md)) closed **100%** — centralized user-pointer validation at the syscall boundary, randomized TCP ISNs, overflow-safe filesystem arithmetic, virtio-RX bounds, **fail-closed TLS** (no trust anchor → connection refused), constant-time signature compares, and software crypto backends (bare-metal-safe, no AVX). **EuroAgent Zero-Trust depth:** **just-in-time capability elevation** (elevated caps grant per-action, auto-revoke on completion), deterministic **behavioral anomaly detection** (probing / drift / rate-spike alerts over the audit stream), and **PCR-sealed secrets** (bound to the measured-boot state — a tampered boot can't unseal). **EuroID end-to-end:** the user store + Argon2id hashes + tamper-evident audit now **persist across reboots**, login runs on **Argon2id (not SHA-256)**, must-change-password is enforced with a self-service `chpasswd`, and an interactive **GUI lockscreen** gates the desktop session against EuroID. The agent audit trail is now **persisted to an append-only on-disk log** (survives reboot).

> **Phase 2B — networking, coreutils & the agent-first runtime (new, all boot-verified):** a stateful **firewall** (`eurofw`) and a sovereign **VPN** (`eurovpn` — X25519 + HKDF-SHA256 + ChaCha20-Poly1305 forward-secret tunnel, TPM-seeded). A GNU-compatible **coreutils** core (`eurocoreutils`: echo/seq/head/tail/wc/sort/uniq/cut/tr/grep/nl/fold/tac/rev/cat + sha256/512sum/base64/base32/cksum) wired into the shell, plus file ops (cp/touch/stat/truncate). And **EuroAgent** (`euroagent`) — the sovereign answer to Microsoft's Project Solara: agents are WASM modules with a declarative capability manifest, capability-isolated **at the kernel** (not a US cloud), an open **MCP gateway** (JSON-RPC, every call audited), and deterministic intent routing. New shell commands: `eurofw · vpn · euroagent` + the coreutils set.

> **Phase 2 — sovereign platform layer (new, all boot-verified):** real **USB** (xHCI keyboard/mouse + mass storage), **Intel HD-Audio**, a unified **device model** (`eurodevice`), **ACPI + an AML interpreter**, **MSI-X** interrupts (USB + storage), per-CPU run-queues, a **concurrent (RwLock) FS cache**, **transparent fault-driven swap**, and HLT-idle. Security/sovereignty spine: per-file **immutability** + `CAP_IMMUTABLE_ADMIN`, an **append-only audit log**, a **TPM 2.0** driver (measured boot — PCR extend), **ChaCha20 full-disk encryption** with a TPM-derived key, **CoW snapshots + rollback** (`eurosnap`), a declarative **capability-policy engine** (`europol`), an encrypted **secrets vault** (`eurovault`), **kernel crash dumps** (`eurocrash`), and a **SMART system-health engine** (`eurohealth`). New shell commands: `lsdev · audit · eurosnap · europol · metrics · vault · eurocrash · eurohealth`.

> **📖 Companion documents:**
> - **[`docs/TECHNICAL-OVERVIEW.md`](docs/TECHNICAL-OVERVIEW.md)** — extensive technical reference: every subsystem, how it works, the build/run guide.
> - **[`docs/ROADMAP.md`](docs/ROADMAP.md)** — extensive forward roadmap: what's next, why, how, and in what order.
> - **[`NEXT-SPRINTS.md`](NEXT-SPRINTS.md)** — the live per-task sprint board (G–K).
>
> This file is the mid-level status summary; the two `docs/` files go deep.

---

## 1. What is built, and how it works

### 1.1 Boot & kernel
- **Own UEFI bootloader** — boots via UEFI (GOP framebuffer), then calls `ExitBootServices` and runs entirely on its own stack/code. No GRUB, no firmware services afterwards.
- **GDT / TSS / IDT** — own segment + interrupt descriptor tables; exception handlers (#GP, #PF, #DF, breakpoint) and a panic handler (red screen + serial trace).
- **COM1 serial** — full logging that survives `ExitBootServices` (the backbone for bare-metal debugging).
- **PS/2 keyboard & mouse** — IRQ-driven, real input.
- **Observability** — a 512-line in-memory `dmesg` ring, leveled+timestamped logging, and a panic backtrace that an offline symbolizer resolves to function names against the linker map.
- **SMP & HPET** — multiple cores via ACPI + INIT–SIPI–SIPI, each with its own run queue; LAPIC timer + IO-APIC; HPET as a 100 MHz high-precision time source alongside the CMOS RTC.

### 1.2 Memory & paging (`euromm`)
- **Frame allocator** — bitmap physical allocator built from the UEFI memory map; supports contiguous and **aligned** allocation (`allocate_aligned`, so a 2 MiB arena costs exactly 2 MiB, no over-allocation), plus double-free detection + high-water tracking.
- **4-level paging** — the boot address space is **supervisor-only** (kernel); every process gets its **own** page tables where only its 2 MiB arena carries the User bit, split to 4 KiB granularity.
- **W^X + NX** — write-xor-execute enforced on user memory: code pages are read-only-executable, data/heap/stack are writable + `NX` (via `EFER.NXE`). Permissions come from the ELF segments and apply to **every** process type — directly loaded, `fork()`ed (child clones the parent's per-page rights), and `execve()`'d (re-derived from the new image).
- **SMEP / SMAP enforced** — `CR4.SMEP` stops ring 0 ever executing a user page; `CR4.SMAP` stops it touching user memory at all, except inside a short, non-preemptible **AC window** opened per syscall.
- **Kernel heap** — a working `alloc` heap (`Vec`/`String`/`Box`).

### 1.3 Scheduler & processes
- **Preemptive mini-CFS** — calibrated 100 Hz LAPIC timer drives a fair scheduler with full register-saving context switches; per-task state machine (Ready · Sleeping · Blocked-on-channel · Zombie · Dead), `nice` priority, virtual runtime; real `sleep`, wait-channels.
- **fork / exec / wait / pipe** — the full Unix process model; `pipe()` is an in-kernel FIFO for IPC.
- **Concurrent processes** — kernel threads + multiple unmodified musl programs run at once, each with its own kernel stack, TLS (`FS_BASE` per task), and heap.
- **Per-process address spaces** — own `CR3`; the kernel is supervisor-only; one ring-3 program cannot read another's memory or the kernel's. A faulting program is terminated gracefully while the rest keeps running.
- **Threads / pthreads / mutexes** — real `clone()`, unmodified musl `pthread_create`/`join`, `pthread_mutex` on a blocking `futex`.
- **EuroInit** — PID-1-style supervisor; restarts crashed services (anti-storm cap); `eurologd` persists the kernel log to `/var/log/messages`.
- **EuroIPC** — kernel message bus; every message carries the sender's **app identity** and is written to an **audit log**.
- **Stack-overflow detection** — a guard canary at the foot of every kernel stack, checked on each context switch; **plus hardware guard pages**: a shared high-memory region (mapped identically under every CR3) hosts guarded kernel stacks where the page *below* the stack is unmapped, so an overflow faults immediately (`KERNEL STACK OVERFLOW`) instead of corrupting memory.

### 1.4 EuroFS — filesystem (`eurofs`)
- **Copy-on-write + A/B superblock** — existing data is never overwritten until the new data is fully written. The superblock commits as a **generation-numbered A/B pair**: a checkpoint writes only the older slot, so a power loss mid-commit always leaves one consistent copy; mount picks the newest valid generation. Ordered by a real **I/O barrier** — `VIRTIO_BLK_T_FLUSH` forces the disk's own write-back cache to the medium (on checkpoint *and* on clean shutdown) — so a checkpoint is genuinely crash-consistent.
- **Self-healing** — a degraded superblock slot is repaired from the valid A/B copy, both on demand (`fsck repair`) and **automatically on mount**.
- **XXH3 checksums + data-path scrub** — every inode and directory block is checksummed, **and every file carries an XXH3 over its full contents** so bit-rot in a *data* block (outside the inode) is caught: a corrupt block makes `read` fail loudly instead of returning silent garbage, and `fsck`/`scrub` verifies every file's data and reports corruption.
- **File metadata** — inodes record POSIX permissions and a real modification time (fed from the RTC); `ls` shows `-rw-r--r--  size  YYYY-MM-DD HH:MM  name`.
- **Operations** — create/read/write/remove files; `mkdir`/`rmdir`; `rename`/`mv` (renames or moves a file or whole directory, atomically replacing a target file, loop-safe on directory moves).
- **On-disk root** — runs as the real root on a GPT partition over `virtio-blk`, with a write-back block cache. An *installed* OS whose files survive reboots.
- **Multiple disks** — the virtio-blk driver probes **all** attached disks (each its own virtqueue); a second disk is mounted as a separate EuroFS (e.g. `/mnt`), with `df` reporting per-mount total/free. Verified on a 2-disk QEMU harness (root on disk 0, data on disk 1).
- **NVMe** — an own NVMe 1.4 driver: PCI detection, controller init (admin + I/O queues), Identify, **read/write** via PRP, and **SMART** (temperature, % used). Verified end-to-end on QEMU (`-device nvme`): read/write self-test passes, SMART reads 50 °C. (The boot identity map was extended to 1 TiB so high 64-bit PCI BARs — QEMU puts NVMe MMIO at 768 GiB — are reachable.)

### 1.5 EuroNet — networking (`euronet`)
- **IPv4** — own ARP/ICMP/UDP; headers validated on parse (bad checksums **and** IP fragments are rejected, never mis-delivered). The background service replies to inbound **ICMP echo requests** (EuroOS is pingable) and returns a proper **ICMP port-unreachable** (RFC 792) for unsolicited UDP, so closed ports signal correctly instead of black-holing. Conversely, a UDP client that *receives* an ICMP unreachable for its own datagram fails fast (connection refused) instead of waiting out the full timeout.
- **DHCP / DNS** — DHCP lease; DNS resolver with a TTL'd cache + `/etc/hosts`. DNS responses are validated (transaction ID + QR flag + ports, with a varying txid) — **anti cache-poisoning**.
- **IPv6** — NDP, SLAAC (link-local + global), ICMPv6, `ping6` — dual-stack.
- **TCP & HTTP** — own three-way handshake, sequencing, ACKs, teardown; every received segment is **checksum-verified against the IPv4 pseudo-header**. A SYN to a closed port is answered with a proper **RST** (connection refused, RFC 793 §3.4). **Reliability:** the SYN is retransmitted if the handshake is lost, sent data is held in a retransmission buffer and resent until the peer's cumulative ACK covers it, and there's an RFC 6298 **RTO estimator** + RFC 5681 **Reno congestion controller** (slow-start/avoidance/fast-recovery) — so TCP survives packet loss, not just a lossless LAN.
- **HTTPS over own TLS 1.3** (`eurotls`) — X25519, ChaCha20-Poly1305, HKDF; key schedule validated against RFC 8448 vectors; AEAD rejects forged/tampered records; the record layer enforces the RFC 8446 §5.1 length bound.
- **X.509 certificate validation** (`eurotls`) — a from-scratch DER/ASN.1 parser (never panics on untrusted bytes) + signature verification (ECDSA P-256/P-384, RSA-PKCS1 & RSA-PSS with SHA-256/384, Ed25519) + chain path-validation (name chaining, per-step signatures, CA constraints, validity window, SAN/hostname match) anchored to a bundled **EU-first root store**. The TLS client verifies the server's CertificateVerify signature and the full chain before trusting a connection — a MITM presenting a valid-but-wrong certificate is **refused**. Verified end-to-end against live public HTTPS.
- **Background HTTP server** — serves inbound connections on :80 while the desktop stays interactive.
- **POSIX sockets** — unmodified musl programs do real `socket()/connect()/send()/recv()` over TCP and UDP; the shell can `wget` a page straight to EuroFS. **Server sockets**: `bind`/`listen`/`accept` with a passive-open handshake + accept queue (backlog-bounded), so EuroOS can run server applications, not just clients (shell `tcpserve` demo).

### 1.6 EuroGuard — security model
- **Capabilities** — every program runs with explicit, signed, least-privilege rights (`CAP_CONSOLE / PROC_INFO / FILE / NET`). No ambient authority, no `sudo`.
- **Ed25519 verify-before-execute** — every binary's signature is checked before it runs; a tampered binary is refused.
- **Compliance** — least-privilege per component maps to **NIS2 Art. 21**; data minimisation at the system level to **GDPR**; the architecture fits the **EU Cyber Resilience Act**.

### 1.7 Userspace, desktop, toolchain
- **Userspace** — ring-3 isolation, fast `SYSCALL`/`SYSRET`, a growing POSIX-style syscall set; a musl/Linux-ABI compat bridge (clearly a *bridge*, not the identity).
- **EuroDesktop** — own compositor on the EDS light theme; a live **System** window (kernel status) and an interactive **Terminal**; dirty-rect rendering (≈100× cheaper per tick than a full blit).
- **EuroDisplay** — a Wayland-shaped display-server protocol (`wl_surface`/`wl_buffer`-style requests, `wl_seat`/`wl_output`-style events, 12-byte wire format) + a compositor-side surface model (z-order, commit-raises-focus, keyboard/pointer routing, damage). The protocol + model are host-tested; carrying it over Unix-domain sockets and driving the live compositor from it is the remaining integration.
- **EuroToolchain** — compiles C source to stripped, signed ELF64 binaries that the kernel loads.
- **EuroUpdate** — atomic **A/B system slots** with automatic rollback: a staged update is tried a bounded number of boots and, if it never confirms good, the next boot rolls back to the last-known-good slot — a failed update can't brick the machine. `euroupdate apply` verifies the image's **Ed25519 signature** before staging; `status`/`rollback` round it out. (The slot state machine is host-tested; the bootloader's per-slot image selection is the remaining integration.)

### 1.8 Test coverage
188 host tests (run on the host, no VM): **eurofs 65 · euronet 51 · eurotls 43 · euromm 13 · euroupdate 5 · eurosandbox 4 · eurodisplay 12 · eurowasm 8 · eurowl 7 · euroaudio 9 · europrint 5 · eurousb 6 · euroacpi 6**, plus a tier-2 kernel UEFI build + QEMU boot + screenshot (the kernel `sock_poll`/VFS-on-NVMe paths are boot-self-test verified, not host-tested). Clippy clean.

---

## 2. Recent hardening pass (2026-06-03 → 06-04)

| Area | What changed |
|------|--------------|
| Memory safety | W^X for fork/execve; SMEP/SMAP re-enabled with a per-syscall AC window; boot PML4 made supervisor-only |
| Reliability | A/B superblock + real `VIRTIO_BLK_T_FLUSH` barrier; self-healing superblock (auto-heal on mount + `fsck repair`) |
| Filesystem | file metadata (mtime/mode); `rename`/`mv`; `rmdir`; **data-path scrub** (per-file XXH3 → bit-rot in a data block caught on read + `fsck`, no longer silent) |
| Network | TCP checksum validation; IPv4 fragment rejection; DNS response validation (anti-poisoning) + RDRAND-randomised source port; ICMP echo-reply + port-unreachable; TCP RST on closed ports; **token-bucket rate-limit on ICMP/RST error replies (anti-amplification)** |
| TLS | record-length bound (RFC 8446 §5.1); AEAD tamper-rejection test; **X.509 chain validation** (own DER parser + ECDSA P-256/P-384 · RSA-PKCS1/PSS SHA-256/384 · Ed25519 + EU-first root store + CertificateVerify) — MITM cert refused, verified vs live HTTPS |
| Memory use | `allocate_aligned` → exact 2 MiB arenas (≈30 MiB freed across background processes) |
| Site | whitepaper gate + CTA; docs kept current; preview image refreshed |

---

## 3. Next up — sprint task list

Ordered by value × safety. Each item ships with tests (host where possible) + a boot verification + docs.

### Sprint G1 — guarded kernel stacks (extends A2) ✅
- [x] Generalized A2 into a **multi-stack guarded allocator** (`paging::guarded_stack_alloc`, uniform guard+stack units in the shared high region, O(1) region-based `is_stack_guard`).
- [x] **Dedicated page-fault IST** (`gdt::PAGE_FAULT_IST_INDEX`) so an overflow's exception-push lands on a fresh stack, not the exhausted one (no double-fault).
- [x] **AP + scheduler per-task kernel stacks migrated** to guarded stacks (`sched::set_task_guarded_stack`, BSS fallback).
- [x] **Recoverable deliberate-overflow test**: a kernel task recurses into its guard page → the PF handler (on its IST) detects the guarded-stack fault, kills *only* that task (`mark_current_dead` + reschedule), and the kernel reaches the desktop. Verified with `-smp 4`: overflow caught at guard `0x80000…`, 3/3 APs online, 3-window desktop up, 0 faults beyond the intentional one. *(IST `DF_STACK`/`RSP0_STACK` migration — pre-paging, attended — remains optional.)*

### Sprint H5 — real Wayland protocol server 🟡 (core done)
- [x] **`eurowl`** — the *real* Wayland **wire protocol** (`[obj_id][(size<<16)|opcode]` headers + word-aligned args) + a minimal compositor server (`wl_display`/`wl_registry`/`wl_compositor`/`wl_surface` + `xdg_wm_base`/`xdg_surface`/`xdg_toplevel`). A full client handshake → a titled window. 7 host tests. Kernel `[h5]` runs a real handshake in-kernel → renders a **4th live desktop window** (`compositor actief — 4 vensters`), 0 faults. *(follow-on: unmodified libwayland clients over AF_UNIX + wl_shm/fd-passing; X11 server; Flatpak runtime.)*

### Sprint H4 — WASM/WASI runtime 🟡 (core done)
- [x] **`eurowasm`** — a `no_std` no-JIT WASM interpreter (LEB128 + sections; i32/i64 arithmetic+compares, locals, `block`/`loop`/`if`/`br`/`call`+recursion, linear memory, globals). **WASI imports gated on EuroGuard capabilities** (`HostImports` trait). 7 host tests (sum-loop 5050, factorial, capability-gated `fd_write`). Kernel `[h4]` self-test: `run()=55`, `fd_write` allowed with `CAP_CONSOLE` / denied without, 0 faults. **+ container binding** (`[h4-ctr]`): WASI `sock_connect` governed by real **EuroSandbox** container caps + net-scope (allowed / denied-no-CAP_NET / denied-by-scope). *(follow-on: f32/f64; full WASI preview1.)*

### Sprint H3 — in-kernel dynamic linker 🟡 (core done)
- [x] The kernel loads a `DT_NEEDED` shared library into the process arena, reads its dynamic symbol table (GNU_HASH-robust symbol count), and patches the executable's **`R_X86_64_JUMP_SLOT` + `GLOB_DAT`** GOT slots — cross-module symbol resolution + GOT patching, in-kernel. Verified: `dyntest.elf` (dynamically linked) calls `euro_answer()` from `libeuro.so` → `H3: 42`, exit 42, 0 faults. *(remaining: VFS DT_NEEDED resolution in the exec path; busybox/curl breadth against a real libc.so.)*

### Sprint G5 — background data scrubber ✅
- [x] `scrub::run`/`maybe_run` — a paced EuroFS integrity pass (superblock + structure + per-inode data-path XXH3, bit-rot detection) logged to `/var/log/fsck.log` (the EuroVar partition). Runs once at boot + periodically (~60 s) from the desktop tick. Verified: `scrub #1: 54 inodes, 183 blocks, 52 data-verified, 0 errors`, 0 faults.

### Sprint A — finish the security foundation
- [ ] **X.509 certificate-chain validation** (the flagged TLS gap): parse the server cert (DER/ASN.1), check validity dates, then verify the signature chain to a bundled EU/Mozilla trust store. Needed before HTTPS can be trusted against MITM. *(Largest item; needs an RSA/ECDSA verify primitive.)*
- [ ] **Kernel-stack guard pages**: split the relevant huge pages to 4 KiB and leave an unmapped guard below each kernel stack, so an overflow faults immediately instead of being caught only at the next context switch.
- [x] **Randomised DNS source port** (defence-in-depth on top of the txid validation already shipped).

### Sprint B — storage depth
- [ ] EuroFS **scrub repair for the data path** (currently only the superblock self-heals): reconstruct from checksums where redundancy exists; report unrecoverable blocks.
- [ ] **NVMe driver** + SMART read-out + bad-block awareness.
- [ ] **Multiple disks / mountpoints**; `df` per mount.

### Sprint C — network depth
- [ ] **TCP retransmission, congestion control, keepalive** (move from a simple client to a robust one).
- [ ] **Server sockets** (`listen`/`accept`) for the POSIX layer, beyond the cooperative HTTP server.
- [x] **ICMP error generation** — port-unreachable for unsolicited UDP + inbound echo-reply shipped; *(remaining: host-unreachable on no-route, ICMPv6 NDP edge cases).*
- [x] **select/poll multiplexing** (G3) — `net::sock_poll(fds, deadline)` reports readiness over a mixed fd-set (Listen ⇔ accept-queue non-empty; Conn ⇔ rx data or EOF), drives `service()`+`pump` per spin, tick-deadline + spin-cap so it never blocks forever. Boot self-test multiplexes a TCP listener + UDP socket; verified `[g3]`. *(deferred: userspace `poll(2)` syscall, unmodified-C `:80` server, Nagle/`TCP_NODELAY`.)*
- [x] **Unix-domain sockets — `AF_UNIX`** (H1) — `euronet::unix::Switchboard`: a kernel-wide local-socket switchboard (bind/listen/connect/accept/send/recv/readable/close, POSIX-shaped errors, bidirectional byte FIFOs, EOF semantics). Kernel API `net::unix_*`; boot self-test `[h1]` proves a full ping/pong round-trip + EOF-after-close. 8 host tests. Prerequisite for the live display server (H2). *(deferred: AF_UNIX `socket()` over the Linux ABI + SCM_RIGHTS fd-passing.)*
- [x] **Live display server** (H2) — `eurodisplay::server`: a length-prefixed frame protocol carrying `Request`s + compositor metadata (Title/Line) over a byte stream, and `ServerView` translating mapped surfaces → `WindowView`. Kernel `dispserv::DispServer` binds AF_UNIX `/run/eurodisplay.sock`, accepts clients, decodes frames, and emits real `compositor::Window`s. An in-kernel app connects over AF_UNIX (H1) and opens a live 3rd desktop window — verified `[h2]` + `compositor actief — 3 vensters`. 7 host tests. *(deferred: Events back to apps, per-tick live updates, pixel buffers/SHM.)*

### Sprint D — SMP scalability
- [ ] **Finer-grained locking**: syscalls run `IF=0` (safe shared state, no heavy locks) — perfect now, a bottleneck under heavy multi-core I/O. Start designing per-subsystem locks so SMP gains aren't lost to kernel contention.

### Sprint E — compatibility (Phase 3)
- [ ] **glibc-compat** layer (run more unmodified Linux binaries).
- [ ] **Display server** (framebuffer → an X11/Wayland-shaped API) and real **audio / USB** input.

### Sprint F — updates & isolation (Phase 4)
- [~] **EuroUpdate**: atomic A/B system slots with rollback. State machine host-tested (5); F1 kernel integration shipped; **G4**: (a) `slot_config` on a **raw GPT-reserved block** (LBA 40, outside EuroFS) — survives FS corruption, verified persistent across a real reboot; (b) **multi-partition A/B GPT** — `install` lays down EuroOS-A/EuroOS-B/EuroVar/EuroBoot, `/var` mounts the EuroVar partition (verified `[g4]`, root-mount path unchanged); (c) **image→slot-partition write** — `apply` writes the verified image directly to the inactive slot's partition (sector I/O + read-back), verified by self-test. (d) **two-stage `loader.efi`** — a separate UEFI binary (`loader/` crate) is now `BOOTX64.EFI`; reads slot_config, picks the slot, `LoadImage`/`StartImage`s `eurokernel-{A,B}.efi`. Verified end-to-end (`[loader] boot slot A → kernel bring-up → desktop`, 0 faults). *(refinement: unify the loader's slot source with the kernel's raw-block config via BlockIO.)*
- [ ] **Containers / WASM** sandbox using the capability model.
- [ ] Printer support (last, per the original priority matrix).

### Continuous
- [ ] Native-speaker review of the 24-language site copy.
- [ ] Keep `/docs/`, the downloadable preview image, and this file current with each feature batch.

---

*Live site: <https://euro-os.eu> · docs: <https://euro-os.eu/docs/> · whitepaper: <https://euro-os.eu/whitepaper/> · try it: <https://euro-os.eu/try/>*
