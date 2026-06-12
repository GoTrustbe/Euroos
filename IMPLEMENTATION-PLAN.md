# EuroOS — Implementation Plan: Sprints & Tasks

*Canonical actionable plan. Merges the 2026-06-04 implementation plan with work already shipped this session. Pairs with [STATUS.md](STATUS.md) (what is built today) and the broader vision in [ROADMAP.md](ROADMAP.md).*

**Baseline now:** 107 host tests green (eurofs 52 · euronet 35 · eurotls 12 · euromm 8) · kernel builds clean (release) · clippy clean · boots to desktop, 0 unexpected faults.

Status legend: ✅ done · 🟡 partial (core shipped, remainder listed) · ⬜ todo

```
SPRINT A  →  SPRINT B  →  SPRINT C  →  SPRINT D  →  SPRINT E  →  SPRINT F
Security      Storage       Network       SMP           Compat        Platform
Foundation    Depth         Depth         Scale         Layer         Maturity
```

A feature may only start once its dependencies are green. Ordered by **safety × correctness × commercial value**.

---

## ✅ Already shipped this session (was on the plan)

| Item | What landed | Tests |
|------|-------------|-------|
| **A3** Randomised DNS source port | `dns_query` uses a varying ephemeral source port (49152–65535, HPET-seeded) + varying txid; response validated on **both** txid and port | euronet DNS suite |
| **C3 (core)** ICMP error generation | `IcmpError{DestUnreachable(Host\|Port),TimeExceeded}` builder **and** parser; kernel replies to inbound echo (pingable), sends **port-unreachable** for unsolicited UDP, sends **TCP RST** for SYN to a closed port; UDP client fast-fails on a received unreachable | euronet +9 (icmp/tcp) |

These reduce **C3** to its edge-case remainder (below) and complete **A3** to a hardening tail.

---

## Sprint A — Security Foundation *(active)*

*Close every remaining security gap before the public demo and whitepaper publication.*

### ✅ A1 — X.509 Certificate-Chain Validation — **DONE (2026-06-04)** — trusted HTTPS now enforced
EuroTLS 1.3 now validates server certificates: a MITM presenting a valid-but-wrong cert is refused. Closed the one significant gap in the network model. eurotls **12 → 43 host tests**, `no_std`-clean, verified **end-to-end against live public HTTPS** (full 4-cert ECDSA-SHA384/P-384 chain → bundled SSL.com root).

1. ✅ **DER/ASN.1 + X.509 v3 parser** (`x509.rs`) — TLV reader (definite-length, rejects indefinite/non-minimal), TBSCertificate fields, issuer/subject raw Names, validity (UTCTime + GeneralizedTime → epoch), SPKI (EC P-256/P-384 · RSA · Ed25519), SAN dNSNames, basicConstraints(CA). **Never panics** — fuzzed over every truncation + garbage. Wildcard SAN matching (leftmost label).
2. ✅ **Signature verification** (`sig.rs`) — ECDSA P-256+SHA-256 (`p256`), ECDSA P-384+SHA-384 (`p384`), Ed25519 (`ed25519-dalek`), RSA-PKCS1v15 SHA-256/384 + **RSA-PSS** SHA-256 (own PKCS#1/PSS/MGF1 + `crypto-bigint` modexp, ≤4096-bit). All verify real certs + reject tampered msg/sig.
3. ✅ **Chain path validation** (`chain.rs`) — name chaining (issuer↔subject), per-step signatures, `basicConstraints` CA on issuers, validity window, leaf SAN match, anchors at the first cert whose issuer is a trusted root (handles cross-signed chains). Real-world SSL.com chain regression test.
4. ✅ **Trust store data** — 30-root EU-first bundle (ISRG X1/X2, DigiCert, GlobalSign, D-TRUST, QuoVadis, Sectigo/USERTrust, SSL.com, Comodo, Buypass, SwissSign, Certigna) as `&'static` DER in `kernel/src/tls_roots/`.
5. ✅ **CertificateVerify** — server's handshake signature over the transcript verified with the leaf key (ECDSA P-256/P-384, RSA-PSS, Ed25519 by SignatureScheme), per RFC 8446 §4.4.3.
6. ✅ **Wired into `Tls13Client`** — `set_trust_anchor(now, roots)`; stores the full chain; kernel `https_get` passes the bundled roots + RTC epoch; on failure the handshake aborts with a logged reason; on success continues.
7. ✅ **Boot verification** — live `https://example.com/` validates and returns its page (`versleuteld ✓`), 0 faults; validation correctly *refused* the connection until the right root was bundled (proof of enforcement).

*Optional later hardening: send an explicit TLS Alert (`bad_certificate`/`unknown_ca`) on failure rather than a silent abort; expand the root bundle; pathLen/keyUsage enforcement.*

### 🟡 A2 — Kernel-Stack Guard Pages — **mechanism DONE (2026-06-04); per-stack migration remaining**
Canaries detect overflow only at the *next* context switch; a guard page faults *immediately*, deterministically.

**✅ Done (boot-verified, 0 faults):**
1. ✅ **Shared high region** — `PML4[1]` (512 GiB–1 TiB) is now shared across the boot PML4 *and every per-process PML4* (`paging::HIGH_PDPT`), so a high-memory mapping is identical under any CR3. *(Also closes the B2 gap: NVMe MMIO at 768 GiB is now reachable under a process CR3.)*
2. ✅ **Guarded-stack allocator** (`setup_guarded_stack`) — carves the first 1 GiB-huge entry of the shared high PDPT into a fine-grained PD→PT and maps a 16 KiB stack on **real allocated frames** with one **unmapped guard page** below it. Consistent in all address spaces. Non-destructive self-test confirms: stack writable, guard not-present.
3. ✅ **#PF detection** — a ring-0 fault in a guard page → `KERNEL STACK OVERFLOW` (immediate, deterministic) instead of silent corruption / late canary.

**⬜ Remaining:**
4. ⬜ Migrate the **actual** kernel stacks (`sched::STACKS[]`, IST `DF_STACK`, `RSP0_STACK`, AP stacks) onto guarded high stacks (one per stack, parameterized base) so every kernel stack is protected — and a deliberate-overflow test confirms the guard fires (destructive → needs a recoverable harness). The existing canary still guards the static stacks meanwhile.

**Tasks**
1. Inventory every kernel stack: task spawn, fork child stack, IST interrupt stacks (TSS).
2. Unmapped 4 KiB guard page below each stack (lowest address; stack grows down).
3. `map_split_2mib_to_4kib(addr)` utility; TLB shootdown on all cores after split (INVLPG / CR3 reload).
4. #PF handler: if fault addr ∈ guard range → print `KERNEL STACK OVERFLOW — task <id>` → hard halt (not recoverable).
5. IST entry 1 for #DF gets its own stack + guard page.
6. Audit interrupt path depth (keep < ~512 B over the task stack).

**Tests:** guard unmapped after alloc; base 4 KiB-aligned; split preserves mappings; split issues shootdown. **Boot:** task that overflows on purpose → expect `STACK OVERFLOW` in serial.
**Deps:** none (parallel with A1). **Size:** ~200–400 lines.

### ✅ A3 — Randomised DNS Source Port — **DONE (2026-06-04)**
- ✅ Varying ephemeral source port + txid, validated on response.
- ✅ **RDRAND hardware entropy** (`rand_u64()`) now feeds both the DNS txid and source port, with an RDTSC+HPET+counter fallback when RDRAND is absent. A spoofer must blind-guess 16-bit txid **and** ~14-bit port from real hardware entropy.

*Optional later: `bind(port=0)` ephemeral assignment in the POSIX socket layer + UDP-socket-table conflict avoidance (the resolver path no longer needs it).*

---

## Sprint B — Storage Depth

*Bring EuroFS from "correct" to "production-grade" across multiple disks and real hardware.*

### 🟡 B1 — EuroFS Data-Path Scrub & Repair — **detection DONE (2026-06-04); repair remaining**
Superblock already self-heals; **data-block bit-rot is now detected** (was previously silent — only the inode block was checksummed, not the extent *contents*).

**✅ Done (eurofs 52 → 54 host tests, format change backward-compatible):**
1. ✅ **Per-file data checksum** — every file carries an XXH3 over its full contents in the inode (unused gap @ offset 56; `0` = legacy → skip, so old images still mount).
2. ✅ **Loud reads** — `read_data` verifies the checksum; a corrupt data block returns `Corruption` instead of silent garbage.
3. ✅ **Data-path scrub** — `scrub`/`fsck` reads every file's data, verifies the checksum, and reports bit-rot (new `data_verified` count, shown in the shell `fsck`/`scrub` output). Survives remount.

4. ✅ **Repair reporting** — single CoW disk has no redundant copy (old versions freed after checkpoint), so corrupt data is reported as **unrecoverable** (`data_unrecoverable` count, shown in `fsck`); the `repair_block(lba, good)` trait interface is defined for the future mirror path (B3) — returns `Unsupported` on one disk.

**⬜ Remaining:**
5. ⬜ **Background scrubber task** (nice +19) via EuroInit; iterate in disk order; rate-limit (~10 MB/s); log to `/var/log/fsck.log`. *(Needs a rootfs handle in a task context.)*
6. ⬜ Real reconstruction arrives with **B3** mirrors (`repair_block` fills in).

**Size:** ~300 done; background scrubber task remaining.

### 🟡 B2 — NVMe Driver — **driver DONE (2026-06-04); EuroFS-on-NVMe remaining**
EuroOS now drives a real NVMe controller end-to-end (`kernel/src/nvme.rs`).

**✅ Done (verified on QEMU `-device nvme`):**
1. ✅ **PCI detection** — match class `01:08:02`; read the 64-bit MMIO BAR; enable bus-master + memory space. *(Legacy PCI config access suffices; full ECAM not needed.)*
2. ✅ **Controller init (NVMe 1.4)** — reset (CC.EN=0 → RDY=0); configure AQA/ASQ/ACQ; CC.EN=1 → RDY=1; **Identify Controller** (model string) + **Identify Namespace** (capacity + LBA size).
3. ✅ **I/O queues** — create I/O CQ + SQ via the admin queue.
4. ✅ **Read (0x02) / Write (0x01)** with a PRP data buffer; **read/write self-test passes** (pattern written + read back at LBA 1000).
5. ✅ **Completion polling** (phase-tag), no interrupts needed.
6. ✅ **SMART** — Get Log Page 0x02 → composite temperature (323 K / 50 °C) + % used.
7. ✅ `read_sectors`/`write_sectors`/`capacity_sectors`/`present` — ready to back a `BlockDevice`.
8. ✅ **High-BAR fix** — QEMU placed the NVMe MMIO at **768 GiB**; the boot identity map was extended (`PML4[1]` → 512 GiB–1 TiB) so high 64-bit PCI BARs are reachable.

**⬜ Remaining:**
9. ⬜ Wrap NVMe in a `RootBlk`-style `BlockDevice` and mount an EuroFS on it (mirror the B3 second-mount path); per-process page tables need `PML4[1]` too if NVMe I/O runs under a process CR3.
10. ⬜ Bad-block table; MSI-X interrupts; multiple namespaces.

**Deps:** none. **Size:** ~360 lines done.

### 🟡 B3 — Multiple Disks & Mountpoints — **multi-disk DONE (2026-06-04); VFS routing remaining**
EuroOS now drives **multiple real disks**, each with its own filesystem.

**✅ Done (verified with a 2-virtio-blk harness `scripts/run-multidisk.py`):**
1. ✅ **Multi-device virtio-blk driver** — probes ALL virtio-blk PCI devices (≤ `MAX_BLK`), each with its own virtqueue + buffers; device-indexed `read_io_dev`/`write_io_dev`/`flush_dev`/`capacity_sectors_dev`/`device_count`. Backward-compatible globals → device 0.
2. ✅ **Device-aware `RootBlk`** — `disk_on(dev, …)`; device 0 via the block cache, further disks via direct uncached I/O + per-device FLUSH.
3. ✅ **Second mount** — a separate EuroFS is mounted on virtio-blk 1 at `/mnt`; boot self-test writes + reads a file on disk 1 ✓.
4. ✅ **`df`** — per-mount total/free from each superblock (logged at boot for `/` and `/mnt`). 0 faults; the RAM-mode boot is unaffected (only activates when a 2nd disk is present).

**⬜ Remaining:**
5. ⬜ **VFS path-routing** — a mount table so the shell (`ls`/`read`/`write`) routes `/mnt/*` → disk 1 by longest-prefix; cross-mount rename → `EXDEV`. *(Shell currently holds a single root fs; this is a shell/VFS refactor.)*
6. ⬜ GPT parse of **all** partitions + `/etc/fstab` read by EuroInit; interactive `mount`/`umount`/`df` commands.

**Deps:** B2 adds NVMe as another `BlockDevice`. **Size:** ~300 done; VFS routing remaining.

---

## Sprint C — Network Depth

*From "client-capable" to "server-capable".*

### 🟡 C1 — TCP Retransmission, Congestion Control & Keepalive — **core DONE (2026-06-04)**
TCP now survives packet loss, not just a lossless LAN.

**✅ Done (euronet +5 host tests):**
1. ✅ **RTO estimator** (`euronet::tcpcc::RttEstimator`) — RFC 6298 SRTT/RTTVAR/RTO, min 1 s, exponential backoff capped 60 s. Karn's algorithm (caller skips retransmitted samples).
2. ✅ **Reno congestion controller** (`RenoCc`) — slow-start, congestion avoidance (≈MSS²/cwnd per ACK), timeout reset, 3-dup-ACK fast-retransmit/recovery. All host-tested.
3. ✅ **Retransmission wired into `TcpConn`** — SYN retransmitted if the handshake is lost (≤4 tries); sent data held in a retransmission buffer, cumulative-ACK tracked (`snd_una`, wrapping-aware), unacked segments resent (bounded). Verified: HTTP+HTTPS still work, 0 faults.

**⬜ Remaining (follow-up, need a bulk-transfer / long-lived use case):**
4. ⬜ Drive the client's send-pacing from `RenoCc.cwnd()` (matters for bulk transfer; the synchronous request/response client sends the whole request at once today).
5. ⬜ Keepalive probes (`SO_KEEPALIVE` etc.) — the synchronous client has no idle periods yet; relevant once C2 server sockets hold long-lived connections.
6. ⬜ Nagle / `TCP_NODELAY`.

### 🟡 C2 — POSIX Server Sockets (`listen`/`accept`) — **API DONE (2026-06-04); e2e harness pending**
Userspace daemons can now do passive open.

**✅ Done:**
1. ✅ **Passive open** — `TcpConn::accept_from` completes a server-side handshake (SYN-ACK + wait ACK, randomised ISN, retransmit), returning an established server socket; buffers a piggybacked request.
2. ✅ **LISTEN state + API** — `Sock::Listen { port, backlog, queue }`; `sock_bind`/`sock_listen`/`sock_accept`. `accept()` blocks (tick-deadline) draining `service()` until a connection arrives, then allocates a new socket fd.
3. ✅ **`service()` routing** — an inbound SYN to a port with a LISTEN socket completes the passive open and enqueues it (backlog-bounded); else falls through to RST. Shares the exact mechanism as the shipped background `:80` server.
4. ✅ **`tcpserve` demo command** (shell) — listen → accept → read request → reply → close.
5. ✅ Builds clean; **boot-healthy** with the changes (0 faults, desktop renders, existing net + background server unaffected).

**⬜ Remaining:**
6. ⬜ A non-fragile end-to-end test of the generic API with an external client (the GUI-keystroke harness `test-tcpserve.py` exists but is unreliable headless — shell-prompt timing + focus). Best: a non-blocking auto-listener served by the desktop loop, or a hostfwd test that waits for a readiness marker.
7. ⬜ `select()`/`poll()` multiplexing; rewrite the `:80` server as an unmodified userspace C binary.

### ✅ C3 — ICMP Errors & ICMPv6 NDP — **DONE (2026-06-04)**
- ✅ ICMP port-unreachable generation, inbound echo-reply, TCP RST on closed ports, ICMP-error parse + UDP client fast-fail.
- ✅ **ICMP/RST rate limiting** — a token-bucket (`euronet::ratelimit::TokenBucket`, 3 host tests) caps error replies to 20/s+20-burst, so EuroOS can't be abused as a reflector/amplifier via spoofed sources.
- **N/A** ICMP Host-Unreachable: only a *router* emits it on a forwarding failure; EuroOS is a host, not a router.

*Optional later: ICMPv6 NDP edge cases (neighbor-cache expiry, DAD) — current SLAAC/NDP works; these are robustness nice-to-haves.*

---

## Sprint D — SMP Scalability

### 🟡 D1 — Finer-Grained Kernel Locking — **D1a (profiling) DONE (2026-06-04); locking refactor remaining**
Syscalls run `IF=0` → one core in the kernel at a time; 8+ cores lose SMP gains under concurrent I/O.

**✅ Done:**
- ✅ **D1a — Profiling** (`ring3.rs`) — an RAII HPET timer around `syscall_dispatch` accumulates per-syscall count + total ns (covers native *and* Linux-ABI paths). `syscallprofile`/`sprof` shell command shows the top syscalls by total time — exactly the hot-path inventory needed before locking. Passive (no semantic change), boot-verified.

**Phased (remaining):**
- **D1a — Profile (no code):** HPET timestamps around syscall entry/exit → `/proc/syscall_profile`; find hot paths + measure contention.
- **D1b — Per-subsystem spinlocks:** per-mount EuroFS lock; per-connection EuroNet lock; per-CPU run-queue locks; per-channel EuroIPC locks. FS-only and net-only syscalls then run in parallel.
- **D1c — Lock-free hot paths:** kmsg ring (atomic producer/consumer); block cache (RwLock); socket table (read-heavy concurrent map).
- **Rules:** no lock held > ~10 µs; never hold a lock across I/O; fixed documented lock order.

**Tests:** concurrent FS+net syscalls no deadlock; lock-hold under threshold; per-CPU run-queue needs no cross-lock; 4-thread FS+TCP stress.
**Deps:** after B1 + C1 (so contention is known). **Size:** ~600–1000 lines.

---

## Sprint E — Compatibility Layer

### 🟡 E1 — glibc-Compat Layer — **Option 1 broadly DONE**
- ✅ **Option 1 (syscalls)** — the kernel already handles ~50 Linux syscalls incl. `mmap`(9, anonymous), `brk`(12), `mprotect`(10), `arch_prctl`, `getpid/getppid`, `uname`(63), `clock_gettime`(228), `getrandom`(318), `set_tid_address`, futex, clone, etc. Unmodified static musl programs run to completion at boot (`/bin/hello` → `exit=0`).
- ⬜ **Breadth**: bundle + run a real multi-call tool (busybox applet / curl / sqlite) to find the next missing syscalls. *(Incremental, boot-testable.)*
- ⬜ **Option 2** — minimal ELF dynamic linker (ld.so): load `.so` from EuroFS, PLT/GOT resolution, `mmap(MAP_FIXED)`. *(Large; for dynamically-linked binaries.)*

**Deps:** after C2.

### 🟡 E2 — Display Server API — **protocol + surface model DONE (2026-06-04); transport remaining**
A Wayland-shaped display server in front of the compositor.

**✅ Done (new `eurodisplay` crate, **5 host tests**, no_std-verified in the kernel):**
1. ✅ **Protocol** — `Request` (`CreateSurface`/`Attach`/`Commit`/`Move`/`Destroy`, mirroring `wl_surface`/`wl_buffer`) + `Event` (`Configure`/`Key`/`Pointer`/`FrameDone`, mirroring `wl_seat`/`wl_output`), with a fixed 12-byte wire encoding (`encode`/`decode`, round-trip + garbage-rejection tested).
2. ✅ **Compositor surface model** (`Display`) — z-ordered surface registry, commit-raises-to-front + focus, **input routing** (keyboard → focused surface; pointer → top-most hit with surface-local coords), damage tracking. Host-tested; a kernel boot self-test drives a surface lifecycle (`scene=1, focus=Some(1)`).

**⬜ Remaining:**
3. ⬜ **Transport** — carry `Request`/`Event` over **Unix-domain sockets** (prerequisite: add `AF_UNIX` to EuroNet/EuroIPC); buffers as shared memory via EuroIPC.
4. ⬜ **Live wiring** — drive the EuroDesktop compositor from `Display::scene()` so real apps draw windows. *(The compositor already does dirty-rect rendering; this connects the protocol to it.)*

**Deps:** Unix-domain sockets. **Size:** ~220 lines done.

---

## Sprint F — Platform Maturity

### 🟡 F1 — EuroUpdate: Atomic A/B System Slots — **logic+integration DONE (2026-06-04); bootloader slot-switch remaining**
A failed update can never brick the device — the industry-standard A/B mechanism.

**✅ Done (new `euroupdate` crate, **5 host tests**, boot-verified):**
1. ✅ **Anti-brick state machine** (`crates/euroupdate`) — `SlotConfig` with `stage_update` → `on_boot` (decrement tries, boot next slot) → `mark_good`, and **automatic rollback** once tries are exhausted without a mark-good. Manual `rollback`. Serialized to a 32-byte block with magic + Fletcher checksum (rejects corruption).
2. ✅ **Boot integration** (`kernel/src/update.rs`) — at boot, reads `/boot/slot_config` (or `initial`), runs `on_boot`, logs the slot decision + persists; on reaching the desktop, `mark_good` confirms the slot. Verified: `boot van slot A … slot A bevestigd GOED`, 0 faults.
3. ✅ **Signed `apply`** — `euroupdate apply <image>` verifies the image's **Ed25519 signature** (`<image>.sig`, via EuroGuard's key) before writing to the inactive slot + staging the update; refuses an unsigned/tampered image. `status` / `rollback` shell commands too.

**⬜ Remaining:**
4. ⬜ Real **A/B GPT partitions** (slot A/B + `/var` + `/boot`) instead of slot *files*; the update writes the partition + read-back checksum.
5. ⬜ The **UEFI loader** must read `slot_config` and load the kernel from the chosen slot (today the kernel runs the slot logic; the loader doesn't pick the image yet).

**Deps:** B2/B3 (done). **Size:** ~360 lines done.

### 🟡 F2 — Container / WASM Sandbox — **container DONE (2026-06-04); WASM runtime remaining**
EuroGuard-native containers (no Linux namespaces).

**✅ Done (new `eurosandbox` crate, **4 host tests**, boot-verified):**
1. ✅ **Container policy** (`crates/eurosandbox`) — `Container { root, caps, net }`: `effective_caps` (intersection — a container can only *shrink* rights), `allow_connect` (scoped net), and **traversal-safe `resolve`** (chroot semantics — `../` can never escape `/containers/<name>`). Host-tested incl. classic escape attempts + prefix-sibling rejection.
2. ✅ **Kernel integration** (`kernel/src/container.rs`) — registry + `create`/`list`/`run`; boot self-test verified: a process loses **CAP_NET** inside the container and `../../../etc/passwd` resolves to `/containers/demo/etc/passwd` (contained). Shell `container create/list/run`.

**⬜ Remaining:**
3. ⬜ Enforce the container scope *inside the syscall path* — route a contained process's file-open/connect through `resolve`/`allow_connect` (needs a process→container binding).
4. ⬜ **WASM runtime** — a minimal WASM interpreter (no JIT) with WASI mapped to EuroGuard caps (`wasi_fs`→CAP_FILE, `wasi_sock`→CAP_NET). *(Large standalone build.)*

**Deps:** C2 (done). **Size:** ~250 lines done.

---

## Continuous — every sprint
- **Docs:** update `/docs/` after each feature batch; keep STATUS.md + whitepaper test count current.
- **Tests:** ≥3 host tests per feature before merge; boot verification after every kernel change; stay clippy-clean.
- **Security review:** every new syscall → capability enforcement; every new parser → random-byte fuzz; every new write path → power-loss (interrupted-write) test.

---

## Dependency graph
```
A1 (X.509)          ───────────────► C1 (indirect)
A2 (guard pages)    ─ standalone
A3 (DNS port) ✅core ─ hardening tail standalone

B1 (FS scrub)       ───────────────► B3 (repair interface)
B2 (NVMe) ─ PCIe ECAM ─────────────► B3
B3 (multi-disk)     ─ after B1 + B2

C1 (TCP retrans)    ───────────────► C2 (reliable server sockets)
C2 (server sockets) ───────────────► E1 (more apps)
C3 ✅core           ─ edge-case remainder standalone

D1 (SMP locking)    ─ after B1 + C1 (contention known)
E1 (glibc)          ─ after C2        E2 (display) ─ after E1
F1 (EuroUpdate)     ─ needs B2 or B3  F2 (containers) ─ needs C2
```

## Remaining-effort overview
| Sprint | Remaining features | Est. lines | Tests to add |
|--------|--------------------|-----------:|-------------:|
| A | A1 X.509, A2 guard pages (+A3 tail) | ~1100 | ~16 |
| B | B1 scrub, B2 NVMe, B3 multi-disk | ~1300 | ~18 |
| C | C1 TCP retrans, C2 server sockets (+C3 tail) | ~1050 | ~14 |
| D | D1 SMP locking | ~800 | ~6 |
| E | E1 glibc, E2 display | ~600 | ~8 |
| F | F1 EuroUpdate, F2 containers | ~1200 | ~12 |
| **Total** | **~11.5 features** | **~6050** | **~74** |

---

## Recommended next step
**A1 (X.509)** is the highest-value remaining item — the only significant security gap, and it gates a trustworthy public HTTPS demo + whitepaper claim. It's the largest single item (~800–1200 lines) and benefits from a focused, attended run. **A2 (guard pages)** is the best parallel/standalone pick and is much smaller. While A1 is in flight I can knock out the **A3 tail** and **C3 remainder** as safe, incremental, host-tested changes.

---

*EuroOS · euro-os.eu · pairs with STATUS.md + ROADMAP.md*
