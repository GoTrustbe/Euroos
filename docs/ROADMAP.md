# EuroOS — Build Roadmap

*What is next to build, why it matters, how it will be done, and in what order.*

**Status as of 2026-06-05.** This roadmap is the forward-looking companion to **`TECHNICAL-OVERVIEW.md`** (what exists today) and **`NEXT-SPRINTS.md`** (the live per-task board). Every item below is scoped to ship the EuroOS way: **host tests where possible + a QEMU boot-verify + docs**, with no MVP shortcuts.

---

## Legend

- **Kind:** `N` new subsystem · `R` rework/extend existing · 🧪 low-risk, high day-to-day value · 🔒 security/robustness-critical, attended · 🏗️ large architectural lever.
- **Status:** ✅ done · 🟡 core done, remainder staged · ⬜ not started.
- **Verify:** how "done" is proven.

---

## 0. Where we are

**Phase 1 (foundations) and the high-value Phase-2 items are complete.** Done and verified: the security foundation (X.509 chain validation, signed code, guarded-stack mechanism, W^X/SMEP/SMAP), storage depth (EuroFS CoW + A/B superblock + scrub, NVMe, multi-disk VFS), network depth (TCP/IP + IPv6 + TLS 1.3 + DHCP/DNS, ICMP, select/poll), the process model (fork/exec/wait/signals/SMP), the app layer (AF_UNIX, a live display server), and update durability (A/B slot config on a raw block).

**The remaining work splits into five themes**, ordered below by value × safety. The honest truth about what's left: the *easy, fully-verifiable wins are largely banked*. What remains is either **deeper kernel surgery** (a page-fault IST, a two-stage loader, per-subsystem locking) or **large new subsystems** (a dynamic linker, a WASM runtime, a USB stack, audio, FDE). Each deserves its own focused, attended session.

---

## 1. Finish the started cores (highest leverage, smallest surface)

Two foundation items are at "🟡 core done" — the mechanism is proven and partly deployed; completing them is bounded, high-robustness work.

### G1 — Migrate *all* kernel stacks to guarded stacks ✅ `R 🔒`

- **Done (this cycle):**
  1. **Multi-stack guarded allocator** — `paging::guarded_stack_alloc`, uniform 5-page units (1 unmapped guard + 4×4 KiB stack) in the shared high region, O(1) region+modulo `is_stack_guard`.
  2. **Dedicated page-fault IST** (`gdt::PAGE_FAULT_IST_INDEX`) so an overflow's exception-push lands on a fresh stack instead of the exhausted one — no double-fault.
  3. **AP + scheduler per-task kernel stacks migrated** to guarded stacks (`sched::set_task_guarded_stack`, BSS fallback).
  4. **Recoverable deliberate-overflow self-test** — a kernel task recurses into its guard page; the `#PF` handler (on its IST) detects the guarded-stack fault, kills *only* that task (`mark_current_dead` + reschedule), and the kernel runs on.
- **Verified** with `-smp 4`: overflow caught at guard `0x8000019f70` and killed, 3/3 APs online running per-CPU schedulers on guarded stacks, the S2 priority tasks ran on guarded stacks, the 3-window desktop came up, **0 faults beyond the intentional one**.
- **Optional remainder:** migrate the IST stacks (`DF_STACK`/`RSP0_STACK`) themselves — configured pre-paging, so it needs TSS IST re-pointing + TR reload after paging (a mistake triple-faults; attended only). Low marginal value since those stacks are fixed-size and short.

### G4 — Real atomic A/B updates (two-stage) 🟡 `R 🔒`

- **Done:** `slot_config` on a raw GPT-reserved block (LBA 40, outside EuroFS), surviving FS corruption; verified persistent across a real reboot. The anti-brick state machine runs on durable storage.
- **Done (this cycle):**
  1. **Multi-partition GPT** — `gpt::install` lays down **EuroOS-A**, **EuroOS-B**, **EuroVar**, **EuroBoot** partitions; `find_partition_by_name` selects them; the root-mount path is unchanged (slot A = first EuroFS). `/var` mounts the EuroVar partition. Verified `[g4]` (4 partitions written, `/var` routes), 0 faults.
  2. **Image writes to partition block-ranges** — `euroupdate::apply` writes the verified image directly to the inactive slot's partition (sector I/O + first-sector read-back), FS-file fallback only if the layout is absent. Verified by boot self-test (`[g4] slot-image-write … ✓`).
  3. **Second-stage `loader.efi`** — DONE: a separate UEFI binary (`loader/`, its own crate) is now `BOOTX64.EFI`. It reads `slot_config`, picks the boot slot, and `LoadImage`/`StartImage`s that slot's kernel (`eurokernel-A.efi` / `eurokernel-B.efi`, both packed in the ESP). Verified end-to-end: `[loader] slot_config → boot slot A` → `LoadImage + StartImage` → `[euro] EuroKernel bring-up` → full boot to desktop with all self-tests, 0 faults. The Android/ChromeOS two-stage model.
- **Loader-owned attempt-counter + auto-rollback — DONE (Sprint 3, 2026-06-13):** the loader now runs `SlotConfig::on_boot()` itself (decrement the attempt counter, or roll back to the last-good slot when attempts are exhausted) and writes the updated `\slot_config` back to the ESP *before* `StartImage` — the real ChromeOS/Android two-stage model. The anti-brick guarantee now holds even for a kernel that never boots. Verified: `update-test.py` RUN3 → `[loader] on_boot: B tries 3 → 2` → boots slot B. The kernel marks a confirmed-good slot back to the ESP via a new memory-light sectored FAT32 read/modify/write (`eurofat::sectored`, host-tested); the full decrement→exhaust→auto-rollback→mark-good cycle is proven on the **real on-disk ESP** by `[upd4]`. **Ed25519 verify-before-activate** is proven with a genuine `dev.key`-signed image by `[upd3]`. `euroupdate fetch <url>` wires the HTTPS fetch→verify→stage path.
- **Remaining (honest):** kernel mark-good over a non-virtio standalone boot disk (AHCI write path); verity-style block hashing (L3, below).

---

## 2. App ecosystem — run real software (Sprint H remainder) 🏗️

The display server (H2) and Unix sockets (H1) are done. The remaining H-items are what make a third-party application ecosystem possible.

### H3 — `ld.so` dynamic linker 🟡 `R 🏗️`
- **Done (Sprint 1, TLS):** the kernel-as-ld.so now sets up the **static TLS block + `fs_base` + TCB self-pointer** (variant-II) for dynamically-loaded programs, and patches **`R_X86_64_TPOFF64`** (initial-exec) GOT slots. Verified: a freestanding `__thread` PIE runs without musl (`[tls]`, 41→42→exit 42) and a `.so`-provided `__thread` via cross-module IE-TLS resolves (`[tls2]`). Remaining for the libc breadth: ifunc + a syscall-surface pass (below).
A minimal ELF **dynamic linker**: load `.so` dependencies, resolve the PLT/GOT, and run dynamically-linked binaries.
- **Done (in-kernel linker, this cycle):** the kernel loads a `DT_NEEDED` shared library into the process arena (at a sub-offset, with the W^X bitmap merged/shifted), reads its dynamic symbol table — robust to GNU_HASH-only `.so`s by deriving the symbol count from `(DT_STRTAB − DT_SYMTAB)/DT_SYMENT` — and patches the executable's **`R_X86_64_JUMP_SLOT` + `R_X86_64_GLOB_DAT`** GOT slots with the resolved symbol addresses (eager binding). **Verified:** a freestanding dynamically-linked PIE (`dyntest.elf`) calls `euro_answer()` resolved from `libeuro.so` → prints `H3: 42`, exit 42, 0 faults. This is the heart of dynamic linking — cross-module symbol resolution + GOT patching — proven end-to-end.
- **Done (run-by-name):** `ring3::needed_libs` parses `DT_NEEDED` and resolves each `.so` from `/lib` in the VFS; `run_dynamic` was generalized to load N libraries. Verified `[h3-fs]`: `/bin/dyntest` (read from the FS) → its `DT_NEEDED=["libeuro.so"]` is resolved from `/lib/libeuro.so`, linked, and run → `H3: 42`, exit 42.
- **Remaining (large, follow-on) — analysis (2026-06-12):** the **busybox / curl / sqlite** breadth pass against a real `libc.so`, surfacing: (a) **TLS relocations** (`R_X86_64_TPOFF64`/`DTPMOD64`/`DTPOFF64`) — feasible (per-task `fs_base` infra exists), needs a TLS-block setup at link time + offset-exact GOT patching; note the reloc only appears cross-module (ld relaxes IE→LE for a single PIE), so it needs a `.so`-provided `__thread`; (b) **`IRELATIVE`/ifunc** — **blocked by the ring0-eager-link + SMEP design**: the ifunc resolver is code that must run in ring3, so this needs a userspace resolver stub; (c) symbol versioning + many more syscalls. The easy relocations (RELATIVE/JUMP_SLOT/GLOB_DAT) are done; the rest is a dedicated sprint.
- **Why:** the vast majority of real Linux software is dynamically linked. The linker machinery now exists; the breadth pass is the path to running real binaries.

### H4 — WASM / WASI runtime 🟡 `R 🏗️`
A minimal **no-JIT WASM interpreter** with **WASI** mapped onto EuroGuard capabilities.
- **Done (this cycle):** the host-tested **`eurowasm`** crate — a `no_std` no-JIT interpreter: LEB128 + section parser; a stack machine covering i32/i64 arithmetic + comparisons, locals, structured control flow (`block`/`loop`/`if`/`else`/`br`/`br_if`/`return`), `call` + recursion, linear memory (`i32.load`/`store`/`memory.grow`), `drop`/`select`/globals. WASI imports are routed through a `HostImports` trait so each host call (e.g. `fd_write`) is **gated on an EuroGuard capability**. **7 host tests** (loop sum 1..100 = 5050, recursive factorial, capability-gated `fd_write`). The kernel `wasm::selftest` runs a module in-kernel: `run() = 55` and `fd_write` writes a string from WASM linear memory — **allowed with `CAP_CONSOLE`, denied without** — 0 faults.
- **Done (process→container binding):** `wasm::container_selftest` runs the same module under three **EuroSandbox containers** — its WASI `sock_connect` is gated on `Container::effective_caps` (the capability bitmask) **and** `Container::allow_connect` (the network scope). Verified `[h4-ctr]`: *allowed* with CAP_NET + an allowed host, *denied* with CAP_NET revoked, *denied* when the net-scope forbids the host. The WASM sandbox is governed by the real sovereign capability model.
- **Done (AH-3):** the data section is applied to linear memory, so a **self-contained `.wasm`** carries its own data; a **`wasm <file>` shell command** runs a real `.wasm` from the VFS in the no-JIT sandbox with cap-gated WASI (`[wasm2]`). **Remaining (follow-on):** f32/f64 ops; the full WASI preview1 surface (real `fd_read`/`path_open`/`sock_*` wired to EuroFS/EuroNet).
- **Why:** a sovereign, capability-native, architecture-independent app format — the EuroOS-preferred way to ship third-party code safely. **Verified:** a host call runs only when its capability is granted.

### H5 — X11/Wayland bridge (EuroXServer) + Flatpak runtime 🟡 `N 🏗️`
An X11/Wayland server in front of EuroDisplay, plus a Flatpak-style sandboxed app runtime.
- **Done (real Wayland protocol core):** the host-tested **`eurowl`** crate implements the *actual* Wayland **wire protocol** — not the Wayland-*shaped* custom framing of H2, but the real `[object_id][(size<<16)|opcode]` headers + word-aligned argument marshalling — and a minimal compositor server covering `wl_display`, `wl_registry` (global advertisement + `bind`), `wl_compositor.create_surface`, `wl_surface.commit`, and the `xdg_wm_base`/`xdg_surface`/`xdg_toplevel` window-management chain (incl. `set_title` + `configure`). A complete client handshake produces a titled window. **7 host tests.** In-kernel (`[h5]`) a real handshake runs through the server and the resulting window is drawn by the compositor — verified as a **4th live desktop window** (`compositor actief — 4 vensters`), 0 faults.
- **Remaining (large, follow-on):** drive it from *unmodified* libwayland clients over an AF_UNIX socket (H1) — which needs fd-passing + `wl_shm` shared-memory buffers, damage tracking, and frame callbacks; an X11 protocol server (EuroXServer); and the Flatpak-style sandboxed runtime binding apps to EuroSandbox containers.
- **Why:** the biggest ecosystem lever — eventually run Firefox/Chromium/LibreOffice inside EuroGuard sandboxes. The real protocol foundation now exists.

---

## 3. Concurrency, scale & reliability (Sprint J)

### J1 — Per-subsystem locking `R 🔒`
Replace the current global `IF=0` (interrupts-off) critical sections with fine-grained locks: **per-mount** filesystem locks, **per-connection** network locks, **per-CPU** run-queues, **per-channel** IPC spinlocks; a lock-free kmsg ring, a block-cache `RwLock`, and a concurrent socket table. Use the D1a syscall profile to target the hottest paths first.
- **Why:** with SMP working, the global lock is now the scalability ceiling — multi-core gains are lost to kernel contention. This is the work that turns "SMP boots" into "SMP scales."
- **Verify:** parallel multi-core I/O throughput rises; no data races under stress (and the SMP self-tests still pass).

### J2 — NVMe robustness `R 🧪`
A bad-block table (integrating with the EuroFS scrubber), **MSI-X** interrupts instead of completion polling, multiple namespaces, and multiple NVMe controllers.

### J3 — Swap `N 🔒`
Anonymous-page swap to a swap partition/file under memory pressure.

---

## 4. Hardware breadth — bare-metal readiness (Sprint I)

EuroOS runs beautifully in a VM; these subsystems make it a daily driver on real laptops.

### I1 — USB stack `N 🏗️🔒`
An **xHCI** controller driver + **USB HID** (keyboard/mouse) + **USB mass storage**. Replaces PS/2-only input on real machines.
- **Why:** modern hardware has no PS/2. Without USB HID, EuroOS can't take input on most real laptops.

### I2 — Audio `N 🔒`
An **Intel HDA** (or AC'97) driver, a simple mixer, and a playback API.

### I3 — ACPI power management `N 🔒` 🟡
- **Done (Sprint 2):** clean **ACPI S5 soft-off** — `power::shutdown()` writes the firmware-correct SLP_TYP (from the AML-evaluated `\_S5`) to the FADT `PM1a_CNT` port; **boot-verified end-to-end** (`[i3-s5]` + `scripts/shutdown-test.py`: QEMU reports a guest-initiated SHUTDOWN, process exits rc=0). `shutdown`/`poweroff`/`reboot` shell commands. **Remaining:** S3 suspend-to-RAM (wake vector), battery/thermal (`_BST`/`_PSR`/`_TMP`) — hardware-attended.
**S3 suspend/resume**, proper shutdown/reboot states, thermal/battery read-out, lid and power-button events.

### I4 — Printer & scanner `N 🏗️`
**IPP** printing + **SANE**-style scanning. Last per the original priority matrix.

---

## 5. Sovereign platform polish (Sprint K)

The features that complete the "sovereign European OS" promise.

### K1 — Full user & session model `N 🔒` ✅ BUILT (boot-verified `[k1]`)
Beyond `login`/`su`: a full identity authority in the host-tested **`euroid`** crate + the kernel `euroid` module, exposed as the `eurousers` CLI.

- **Sovereign Argon2id** (`euroid::argon2`) — from-scratch Blake2b (RFC 7693) + Argon2id (RFC 9106), validated against the **official RFC 9106 test vector**. Params 64 MiB / t=3 / p=4 / 32-byte TPM-RNG salt, never negotiated down. Passwords are never MD5/SHA1/bcrypt.
- **Data model** — `UserId`/`GroupId` newtypes (never a raw `u32`), `User`/`UserState` (Active/Locked/Expired/Deleted — deleted records are kept for audit), `Group` with the six built-in groups (wheel/audit/net/vault/agent/users), `PasswordRecord` with history (no reuse of the last 12).
- **EuroAuth login flow** — `authenticate()` with **timing-attack prevention** (unknown user runs a dummy Argon2id verify; identical error for unknown-user vs. wrong-password), failed-login lockout (5 attempts), `must_change`, and per-session capability derivation (`effective_caps` = own ∪ groups, then bounded by the EuroPol allow-mask — policy can only reduce).
- **Hash-chain audit log** (P3) — every action serialized as a tamper-evident JSON record where each entry hashes `seq ‖ prev_hash ‖ body`; any edit to a past entry invalidates every later hash. `eurousers audit --verify-chain` reports integrity + root hash. GDPR Art. 32 pseudonymisation (UIDs, not usernames, as the key).
- **`eurousers` CLI** — `add / list / show / passwd / lock / unlock / del / groups / audit [--user|--verify-chain]`, gated on `CAP_USER_ADMIN`.
- **Verification** — 24 host tests (Argon2id RFC vector, policy, history-reuse, `effective_caps`, audit tamper-detection, username validation, timing parity) + a `[k1]` boot self-test that runs the whole chain end-to-end and live-exercises the `eurousers` shell path.
- **Next miles (attended):** persist `/etc/euro/{users,groups,shadow}.db` + `/var/log/euro/audit.log` to EuroFS with the IMMUTABLE/APPEND_ONLY flags across boots; wire `login`/`su`/`passwd` shell commands and the EuroDesktop login screen onto `euroid::authenticate`; TPM-backed (PCR-quote) login.

### K2 — EuroUpdate delivery `R 🧪`
Fetch **signed update images over HTTPS** (now that A1 validates certificates) and feed them into G4's A/B apply — an update server plus stable/beta channels. This is the end-to-end "secure OTA updates" story.

### K3 — Disk encryption (FDE) `N 🔒`
**Full-disk encryption** for EuroFS — sovereign data-at-rest. The security spec lists boot-chain + FDE + signing as hard requirements; this completes the triad.

### K4 — HAL extensions `N`
Multiple framebuffers/displays, GPU-acceleration groundwork, more NIC drivers, RTC/clock-sync.

---

## 6. Immutability subsystem (Sprint L) 🔒

Immutability is not a single feature but an architectural principle spanning EuroFS, EuroGuard, and the system image model. It operates at two levels: **filesystem-level** (files and directories that cannot be modified, renamed, or deleted) and **capability-level** (processes that cannot acquire write access regardless of privilege).

### L1 — EuroFS immutable flag `N 🔒`

A per-inode `IMMUTABLE` attribute, settable by a privileged process or the kernel at image-build time.

- **Semantics:** a flagged inode rejects all write, truncate, rename, unlink, and xattr-write operations — even from root/ring-0 processes — until the flag is explicitly cleared by a holder of `CAP_IMMUTABLE_ADMIN`. The flag is stored in the inode's on-disk metadata and survives remount.
- **Append-only variant (`APPEND_ONLY`):** allows `O_APPEND` writes but rejects seeks + overwrites. Intended for audit logs and kmsg sinks — data can grow but never be altered retroactively.
- **Directory immutability:** a flagged directory rejects `mkdir`, `rmdir`, `rename`, and `create` inside it; existing contents are unaffected unless they are themselves flagged.
- **Verify:** a process with full capabilities attempts to unlink a flagged file → `EPERM`; the scrubber detects and reports any on-disk flag corruption; a flagged audit log can be appended to but not overwritten.

### L2 — EuroGuard `CAP_IMMUTABLE_ADMIN` capability `N 🔒`

Introduce a new capability gate for all immutability management operations.

- **`CAP_IMMUTABLE_ADMIN`:** required to set or clear the `IMMUTABLE` / `APPEND_ONLY` flag on any inode. Absent this capability, `chattr`-equivalent syscalls return `EPERM` — even for processes running as UID 0. This separates "system administrator" from "immutability trustee," which is the correct model for a sovereign OS where the boot chain, not a logged-in admin, should be the authority on what is immutable.
- **Capability drop:** a process can drop `CAP_IMMUTABLE_ADMIN` via the existing EuroGuard `cap_drop` path; once dropped it cannot be re-acquired within that process lifetime.
- **Integration with K1 (users):** per-user EuroGuard policy can permanently exclude `CAP_IMMUTABLE_ADMIN` from user sessions, making user-writable areas structurally separate from system-immutable areas.
- **Verify:** a process without `CAP_IMMUTABLE_ADMIN` cannot unflag a protected inode even when running as UID 0; the kernel audit log records every flag-change attempt with actor identity.

### L3 — Immutable system image partitions `N 🔒 🏗️`

Extend G4's multi-partition GPT model so the active OS partition is mounted read-only and flagged immutable at the partition level, not just the file level.

- **Read-only slot mount:** after the two-stage `loader.efi` (G4) selects the active slot, the slot partition is mounted `O_RDONLY` — the kernel refuses to issue any write I/O to that partition regardless of what userspace requests. `/var`, `/home`, and `/tmp` are separate writable partitions (already in G4's partition plan).
- **Verity-style block hash tree:** a Merkle tree over the slot partition's blocks, computed at image-build time and stored in a reserved area of the partition. On mount, the kernel verifies the root hash against the Ed25519-signed update manifest — any tampered block causes mount to fail and triggers automatic rollback to the other A/B slot. This is the `dm-verity` analogue for EuroOS.
- **Build-time flag pass:** the image builder sets `IMMUTABLE` on all OS-owned inodes (kernel, system services, EuroToolchain binaries) as part of the release pipeline. Developer builds can opt out for local iteration.
- **Verify:** modify a byte in the mounted OS partition from a privileged process → `EROFS`; corrupt a block on disk → mount detects hash mismatch → boots other slot; `euroctl integrity check` reports per-partition hash status.

### L4 — User-accessible immutability API `N 🧪`

Expose immutability controls to unprivileged users for their own files, within their capability scope.

- **User-level flag:** a user can set `IMMUTABLE` on files in their own home directory without `CAP_IMMUTABLE_ADMIN`. The flag is scoped: it prevents modification by other processes (and by that user's own future sessions) but can be cleared by the user themselves — or by an admin holding `CAP_IMMUTABLE_ADMIN`. This models "I want to protect this file from accidental deletion" rather than "this file is a system artifact."
- **Shell commands:** `euroattr +i <file>` / `euroattr -i <file>` (analogous to `chattr +i`); `euroattr +a <file>` for append-only. Integrated into the existing ~45-command shell.
- **EuroDisplay integration (long-term):** a file manager surface (once H5 exists) can show immutable files with a lock badge and prevent drag-to-trash without explicit unprotect.
- **Verify:** a user sets `+i` on a file, then tries to overwrite it from a second shell session → `EPERM`; the user clears `+i` and the write succeeds; a different user cannot clear the flag.

---

## 7. Toolchain & developer experience (Sprint M)

### M1 — EuroToolchain `N 🏗️`
A native EuroOS compiler/linker/debugger target: Rust std support for `x86_64-unknown-euroos`, a minimal `libc`-equivalent (`eurolibc`) so that EuroOS can eventually build itself, and a `gdb`/`lldb` remote stub for kernel-level debugging over serial or virtio-console.
- **Why:** the sovereignty claim is incomplete while the build chain depends entirely on a Linux host. EuroToolchain is also the prerequisite for a self-hosting EuroOS.
- **Verify:** EuroOS builds a userspace binary natively (inside a running EuroOS instance) and executes it.

### M2 — Package manager (europkg) `N 🏗️`
A signed-package format (content-addressed, Ed25519-signed manifest) and a minimal package manager: install, remove, upgrade, dependency resolution. Backends: native EuroFS packages first, WASM packages (H4) as the preferred future format.
- **Why:** without a package manager, software distribution is manual binary drops. This is the distribution layer that makes EuroOS usable at scale.
- **Verify:** `europkg install curl` fetches, verifies, and installs a signed package; `europkg remove` cleanly uninstalls; a tampered package is rejected at the signature check.

### M3 — Reproducible builds `N 🧪`
Deterministic build pipeline: fixed source timestamps, content-addressed intermediate artifacts, a `eurorepro verify <image>` tool that rebuilds from source and compares the binary hash. Pairs with K2 (signed OTA) to make the full chain verifiable: source → binary → signed image → verified update.
- **Verify:** two independent builds of the same tagged release produce bit-identical `eurokernel.img`; `eurorepro verify` reports ✅.

---

## 8. Network completeness (Sprint N)

### N1 — WiFi stack `N 🏗️🔒`
**802.11** (infrastructure mode) + **WPA3-Personal** (SAE handshake) driver, starting with a common chipset (Intel AX200/AX210 via the `iwlwifi`-compatible register interface). Depends on I1 (USB) for USB WiFi fallback path.
- **Why:** modern laptops have no Ethernet port. Without WiFi, EuroOS cannot reach a network on real hardware.
- **Verify:** EuroOS associates with a WPA3 AP, obtains a DHCP lease, and performs an HTTPS GET — all confirmed in the serial log.

### N2 — WireGuard-native VPN `N 🔒`
A clean-room **WireGuard** implementation in `euronet` (the protocol is fully specified and unencumbered): Noise_IKpsk2 handshake, ChaCha20-Poly1305 data path, UDP transport, `wg0` virtual interface exposed through the VFS (`/dev/wg0`). Cryptographic primitives reuse `eurotls`'s existing ChaCha20/Poly1305 and X25519 implementations.
- **Why:** sovereign infrastructure runs on private networks. WireGuard is the correct choice — minimal, formally verified protocol, and the crypto is already in-tree.
- **Verify:** two EuroOS instances form a WireGuard tunnel; ping6 over the tunnel succeeds; a packet capture on the physical interface shows only encrypted UDP.

### N3 — Stateful packet filter `N 🔒`
A kernel-level packet filter (EuroNet firewall): per-interface ingress/egress rule tables, stateful TCP/UDP connection tracking, `eurofw` shell command for rule management. Default policy: deny all inbound, allow established+related.
- **Why:** a sovereign OS that accepts all inbound traffic by default is not sovereign. This is the network boundary enforcement layer.
- **Verify:** a rule blocks inbound TCP on port 22; an existing connection survives a rule reload; `eurofw list` shows the rule table with hit counters.

---

## 9. Sovereign identity & trust (Sprint O)

### O1 — TPM 2.0 driver `N 🔒`
A **TPM 2.0** driver (TIS/CRB interface over MMIO/LPC) exposing: PCR extend/read, key generation and sealing (RSA-2048 / ECC P-256), `TPM2_Unseal` gated on PCR policy. Integrates with K3 (FDE): the disk encryption key is sealed to a PCR set that includes the boot chain hash — it is only released if the system booted the expected kernel.
- **Why:** TPM 2.0 is the hardware root of trust for a verifiable boot chain. Without it, "sovereign" is a software claim; with it, it is a hardware guarantee.
- **Verify:** a sealed blob is unsealed only when the correct kernel boots (measured boot); a kernel tamper changes PCR values → unseal fails → FDE mount fails → rollback.

### O2 — Remote attestation `N 🔒`
A **quote-and-verify** protocol: the EuroOS instance generates a TPM quote (signed over current PCR values + a nonce) that a remote verifier can check against the expected reference values from the signed release manifest. Exposes a `euroattest` shell command and a JSON-over-HTTPS attestation endpoint.
- **Why:** enterprise and government deployments need proof that the OS running on a machine is unmodified. Remote attestation is the mechanism. This is also the basis for zero-trust network admission.
- **Verify:** a reference EuroOS instance generates a valid quote; a tampered instance generates a quote that fails verification; the attestation endpoint returns a signed JSON result.

### O3 — EuroCA trust anchor `N 🔒`
Replace the current 25-entry EU-oriented TLS trust store with a **EuroCA** infrastructure: a self-operated root CA (offline key, hardware-protected) and intermediate CAs for code signing, OTA updates, and attestation certificates. The existing `eurotls` X.509 validator is already capable; this adds the operational key management layer.
- **Why:** today EuroOS trusts third-party CAs for TLS. A truly sovereign OS controls its own trust anchors, especially for the update and attestation paths.
- **Verify:** a certificate signed by EuroCA is accepted by `eurotls`; a certificate signed by an unknown CA is rejected; the root key ceremony is documented and reproducible.

---

## 10. EU compliance & localisation (Sprint P)

### P1 — EU locale support `N 🧪`
Full **CLDR**-based locale support for all 24 EU official languages: collation, date/time formatting (ISO 8601 default), number formatting (comma decimal separator), currency (€), and plural rules. A `eurolocale` library crate, host-testable.
- **Why:** "European OS" with English-only locale support is a contradiction. Locale correctness is a compliance requirement for public-sector deployments.
- **Verify:** date formatting, number formatting, and sort order are correct for NL, FR, DE, and PL locales under host tests.

### P2 — Accessibility layer `N 🧪`
An **AT-SPI2**-equivalent accessibility protocol for EuroDisplay: structured widget trees, focus events, text content exposure, and a screen reader hook. A minimal `euroread` screen reader as the reference consumer.
- **Why:** EU public procurement (EN 301 549) requires accessibility. An OS without it cannot be used in government or education contexts.
- **Verify:** `euroread` announces window focus changes and button labels via the accessibility protocol; a new EuroDisplay client can expose its widget tree without kernel modification.

### P3 — GDPR-native audit log `N 🔒`
A structured, tamper-evident system audit log: every capability grant/revoke, every `execve`, every network connection, and every immutability flag change is written to an `APPEND_ONLY`-flagged EuroFS log (L1). Log entries are JSON with actor identity, timestamp (HPET-based), and action. A `euroaudit` command for querying and exporting.
- **Why:** GDPR Article 5(2) accountability and NIS2 logging requirements. Sovereign infrastructure needs provable audit trails. Also the operational foundation for incident response.
- **Verify:** a complete boot-to-shutdown session produces a parseable audit log; an attempt to retroactively modify a log entry is rejected (`APPEND_ONLY`); `euroaudit export` produces a valid signed JSON archive.

---

## 11. Distribution & governance (Sprint Q)

### Q1 — Installer / live image `N 🧪`
A guided installer: partition layout selection, locale + keyboard setup, user account creation (K1), FDE key enrollment (K3), and EuroCA certificate provisioning. A live-boot mode (RAM-only, no install) for evaluation. The installer itself runs as a signed EuroOS userspace process.
- **Why:** `dd if=eurokernel.img of=/dev/sdX` is not a user-facing installation path. Without an installer, EuroOS cannot be adopted by anyone who is not a kernel developer.
- **Verify:** a fresh x86-64 machine boots the live image, runs the installer, reboots, and reaches the desktop with the configured locale, user, and FDE enabled.

### Q2 — Reproducible builds & release pipeline `N 🧪`
Deterministic build pipeline (fixed timestamps, content-addressed artifacts) and a `eurorepro verify <image>` tool. Integrated with the CI harness (already Woodpecker-compatible by design). Every release tag produces a signed manifest linking source commit → binary hash → Ed25519 signature.
- **Verify:** two independent builds of the same tagged release produce bit-identical `eurokernel.img`.

### Q3 — Open source governance `N`
Licence selection (EUPL-1.2 is the natural choice for an EU-sovereign project), CLA or DCO process, security disclosure policy (coordinated CVE disclosure with a 90-day embargo), and a hardware compatibility list process. These are not code items but are release-blocking for any public launch.

---

## 12. Recommended execution order (updated)

The fastest, safest path to the biggest outcomes:

1. **Finish G1 + G4** — close the two started cores. Highest robustness-per-line; production-critical.
2. **H3 (ld.so)** — dynamic linker; pair with a busybox/curl/sqlite breadth pass.
3. **L1 + L2** — EuroFS immutable flag + `CAP_IMMUTABLE_ADMIN`. Small surface, high security value; unlocks L3 and L4.
4. **J1 (per-subsystem locking)** — convert working SMP into scalable SMP.
5. **I1 (USB) + N1 (WiFi)** — the gate to real-hardware daily use; WiFi depends on USB for the USB-WiFi path.
6. **O1 (TPM 2.0)** — hardware root of trust; pairs with K3 (FDE) for sealed key storage.
7. **H4 (WASM/WASI)** then **H5 (X/Wayland + Flatpak)** — sovereign and compatibility app ecosystems.
8. **K2 (signed OTA) + K3 (FDE) + K1 (users)** — complete the sovereign-platform promise; L3 (verity) follows K3.
9. **L3 + L4** — immutable system partitions + user-facing immutability API.
10. **N2 (WireGuard) + N3 (packet filter) + O2 (attestation)** — network sovereignty and zero-trust foundation.
11. **M1 (EuroToolchain) + M2 (europkg)** — self-hosting and distribution layer.
12. **P1–P3** — EU localisation, accessibility, GDPR audit log.
13. **Q1 (installer) + Q2 (repro builds) + Q3 (governance)** — public launch readiness.
14. **I2–I4, J2–J3, K4, O3 (EuroCA)** — breadth and polish as priorities dictate.

### Dependency notes
- H5 depends on H3/H4. K2 depends on G4 + A1 (✅). L3 (verity) depends on G4 (multi-partition GPT) + K3 (FDE key model). O1 (TPM) depends on I3 (ACPI, for TPM MMIO discovery) or direct MMIO. N1 (WiFi) depends on I1 (USB). O2 (attestation) depends on O1 (TPM). L3 (partition verity) depends on L1 (inode flags) for the build-time flag pass. P3 (audit log) depends on L1 (APPEND_ONLY). M3 (repro builds) depends on Q2 (release pipeline).

---

## 12b. Platform-maturity sprints R–Z + EuroSuite (added 2026-06-05)

Beyond G–Q, two roadmap additions extend EuroOS from a bootable sovereign kernel to a full platform + product. **Per-item module layouts, on-disk formats, data structures and verify steps are in the source docs**; the sprint-board rows are in `NEXT-SPRINTS.md`.

> **Status (2026-06-05):** much of this is now BUILT & boot-verified — R EuroDevice ✅, U EuroVault ✅, immutability/audit (L1/L2/P3) ✅, O1 TPM ✅, K3 FDE ✅, X EuroPol ✅, W EuroObserve ✅, Y EuroCrash ✅, Z EuroHealth ✅, N3 EuroFW ✅, N2 EuroVPN ✅. **EuroCoreutils** is largely landed (`eurocoreutils` crate, CU-0..CU-7 host-tested, wired into the shell, `seq 5` boot-verified). **Sprint AA EuroAgent** has its core landed (`euroagent` crate, AA-1 manifest / AA-2 caps+policy ✅, AA-3 MCP-gateway / AA-4 intent 🟢, boot-verified). Remaining: AA-5 (WASM agent loop + local LLM) + the userspace MCP daemon + reference agents; coreutils find/xargs + pipe-stdin; then K4 GPU, Q1 installer, T EuroContainer, V EuroIDM, EuroSuite.

**Sprints R–Z (platform maturity, kernel/userspace):**
- **R EuroDevice** — unified driver framework & device model (`DeviceTree`/`DriverRegistry`/`trait Driver`, hotplug bus); migrate PCI/NVMe/VirtIO/**xHCI/HDA** onto it. *The base for all future hardware — do early.*
- **S EuroSnap** — CoW snapshots + rollback, integrated with G4 A/B updates (auto-rollback a failed update).
- **T EuroContainer** — OCI containers on EuroGuard capabilities + EuroFS overlay (needs H4 + S).
- **U EuroVault** — capability-gated secrets store, TPM-sealed (needs O1 + P3).
- **V EuroIDM** — enterprise identity (local/LDAP-AD/OIDC), group→capability mapping (needs K1 + TLS + H5).
- **W EuroObserve** — in-kernel lock-free metrics + OpenMetrics/Prometheus + W3C tracing.
- **X EuroPol** — declarative TOML/YAML policy → EuroGuard capability grants, syscall-path enforcement.
- **Y EuroCrash** — kernel crash dumps (mini/full) + recovery boot (needs G1 + L1).
- **Z EuroHealth** — SMART + FS-health + memory diagnostics daemon, feeds W (needs NVMe + scrubber).
- *Deferred:* A3 hypervisor, A5 distributed storage, A10 backup, A11 hw-compat DB (→Q3), A13 AI runtime, A14 sovereign cloud.
- *Order:* R → S → W → X → U → Y → Z → T → V.

**EuroSuite (separate userspace product — `docs/EUROSUITE-PLAN.md`):** a from-scratch Rust office suite (Writer/Calc/Impress) — Universal Document Model + shared engine, OOXML (.docx/.xlsx/.pptx) + ODF + PDF I/O with round-trip Word/Excel/PowerPoint compat, Slint UI, rustybuzz/fontdue rendering, EuroFS-CoW version history + EuroGuard document sandboxing. ~33-week MVP; a multi-month track parallel to the kernel roadmap. Metric-compatible fonts (Liberation/Carlito/Caladea), EUPL-1.2, no MS-font bundling.

**EuroCoreutils (GNU-compatible userland — `docs/EUROCOREUTILS-PLAN.md`):** ✅ *largely built.* The host-tested **`crates/eurocoreutils`** crate (24 host tests) fills the gap, each command a pure `fn(args, input) -> Vec<u8>` tested against expected GNU output and wired into `shell.rs` via a `coreutils()` dispatcher (the last existing file argument is read as stdin). Landed: CU-0 arg-parser, CU-1 trivial (echo/seq/basename/dirname/true/false/yes/arch/nproc/pwd), CU-2 file-ops (cp/touch/stat/truncate — `stat` shows L1 immutability flags), CU-3 text-I/O (head/tail/wc/tac/rev/nl/fold/cat-n), CU-4 transform (sort/uniq/cut/tr), CU-5 grep, CU-6 checksums/encoding (sha256/512/224/384sum, base64/base32, cksum), CU-7 compute/control (printf/expr/test/`[`/numfmt/factor). REMAINING: find/xargs, pipe-stdin for built-ins, and the sovereign extras (`cp --verify` Ed25519, `grep --audit` → P3, checksum `--sign`).

**Sprint AA — EuroAgent (sovereign agent-first runtime — `docs/EUROAGENT-PLAN.md`):** ⭐ 🟢 *core built & boot-verified.* The strategic differentiator vs Microsoft's Project Solara. AI agents run as **WASM modules with a declarative capability manifest**, **capability-isolated at the kernel level** via EuroGuard `AgentCaps` (an agent never exceeds its parent user, then filtered by EuroPol), with **every tool call audited to P3**. A native **MCP gateway** (JSON-RPC over AF_UNIX) exposes EuroOS subsystems (fs/net/vault/display/calendar/spawn) as tools; **EuroDispatch** routes intents to agents; the LLM backend is a **local Ollama-compatible default** (cloud opt-in, key via EuroVault). The trust boundary is the kernel, not a US cloud — full offline operation, EU data residency by construction. **Landed (`crates/euroagent`, 26 host tests, `agent`/`euroagent` shell, `[aa]` boot self-test green):** AA-1 manifest (own no_std TOML parser), AA-2 `AgentCaps` + the effective-cap derivation `(required|granted) ∩ user_caps − EuroPol_denied`, AA-3 MCP gateway (own no_std JSON, 10 cap-gated tools, audit records), AA-4 deterministic intent routing. REMAINING: AA-5 (WASM agent loop + `LlmBackend` local/cloud), the userspace MCP daemon (AF_UNIX socket + real subsystem backends), Ed25519 `.euroa` bundle verification, and the 4 reference agents.

---

## 13. Themes by milestone (updated)

| Milestone | Unlocks | Gating items |
|-----------|---------|--------------|
| **Robust core** | Crash-safe kernel + real A/B OTA | G1, G4, J1 |
| **Immutable by default** | Tamper-proof system files, audit-safe logs | L1, L2, L3 |
| **Runs real software** | Dynamically-linked Linux apps, WASM apps | H3, H4 |
| **Daily driver** | Real laptops (USB, WiFi, audio, suspend) | I1, N1, I2, I3 |
| **Hardware trust** | Verified boot, sealed FDE, remote attestation | O1, K3, O2 |
| **Sovereign platform** | Multi-user, signed OTA, encrypted disk | K1, K2, K3 |
| **Network sovereign** | VPN, firewall, zero-trust admission | N2, N3, O2 |
| **Desktop ecosystem** | Firefox/Chromium/LibreOffice in a sandbox | H5 (after H3/H4) |
| **EU compliant** | 24 languages, accessibility, GDPR audit | P1, P2, P3 |
| **Self-hosting** | EuroOS builds itself, distributes packages | M1, M2 |
| **Operable platform** | Driver framework, snapshots, metrics, policy, crash dumps, health | R, S, W, X, Y, Z |
| **Enterprise-ready** | Secrets vault, federated identity, containers | U, V, T |
| **Sovereign office** | Native Writer/Calc/Impress, OOXML/ODF/PDF | EuroSuite (ES-Core/IO/Writer/Calc/Impress/Int) |
| **Public launch** | Installer, repro builds, OSS governance | Q1, Q2, Q3 |

---

*Pick a sprint or an ID (e.g. "do H3", "finish G4", "do L1", "Sprint N WiFi") to run it next — implemented with host tests, a boot-verify, and updated docs.*

*Live site: <https://euro-os.eu> · technical overview: `docs/TECHNICAL-OVERVIEW.md` · sprint board: `NEXT-SPRINTS.md`*
