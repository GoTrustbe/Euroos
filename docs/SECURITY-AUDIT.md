# EuroOS — Full-Stack Security & Correctness Audit (2026-06-06)

A 5-track parallel audit of the whole code stack (kernel core, storage/FS, crypto/security/net,
userland crates, agent runtime/WASM sandbox). Each finding was verified by reading the actual code
(several reproduced standalone). **What audited clean is as important as what didn't** — the
cryptographic primitives, the capability model, the A/B-superblock crash-consistency, and the CoW
commit ordering all held up.

Severity: **CRITICAL** = memory-unsafe or isolation/secrecy break reachable in normal operation ·
**HIGH** = crash/DoS or secrecy weakness on untrusted input · **MEDIUM/LOW** = soundness/defence-in-depth.

Status legend: ✅ fixed this pass · 🔭 documented, deferred to a focused hardening sprint.

---

## CRITICAL

| # | Where | Issue | Status |
|---|-------|-------|--------|
| C1 | `kernel/src/ring3.rs` syscall layer | No `in_user_arena(ptr,len)` validation: a ring-3 program passes any address to `read`/`write`/`clone`-TID/`futex`; the kernel `copy_nonoverlapping`s into it → arbitrary kernel R/W. *(Mitigated today: executed programs are Ed25519-signed; this is an isolation hole for trusted/buggy code, not arbitrary attacker code.)* | ✅ helper + key copy paths |
| C2 | `kernel/src/virtio_blk.rs` `setup_queue` | No `qsz ≥ 3` guard; a device advertising a tiny queue → OOB descriptor-ring writes. | ✅ |
| C3 | `kernel/src/virtio_blk.rs` read/write | Silent truncation past 4096 B + no LBA-vs-capacity check, both returning success → silent data loss. | ✅ |
| C4 | `crates/eurowifi/src/lib.rs:119` `parse_beacon` | Reads `frame[34..36]` after only a `len ≥ 24` guard → panic on a 24–35-byte beacon (attacker over-the-air). | ✅ |
| C5 | `crates/eurocalc/src/lib.rs` `parse_ref` | `col*26` overflows on a ≥7-letter cell ref → panic. `=ZZZZZZZ1`. | ✅ |
| C6 | `kernel/src/agent.rs` `sandbox_path` | Clamp strips `..`/`.` rather than reject-and-verify; brittle, backend-dependent (no `\`, no canonical prefix check). | ✅ hardened (reject) |

## HIGH

| # | Where | Issue | Status |
|---|-------|-------|--------|
| H1 | `kernel/src/net.rs` `gather_entropy` | TLS ephemeral X25519 secret + ClientHello-random fall back to low-entropy `RDTSC^HPET^counter` when RDRAND is absent — **the live TCG/sandbox path**. The TPM RNG is not consulted. Predictable session keys. | ✅ TPM-seeded |
| H2 | `crates/eurovpn/src/lib.rs` `decrypt` | No replay protection: a captured packet replays forever. | ✅ anti-replay window |
| H3 | `crates/eurofde/src/lib.rs` | Deterministic per-LBA nonce → ChaCha20 keystream reuse when an LBA is rewritten (`C_old⊕C_new = P_old⊕P_new`); no MAC. | 🔭 (needs per-write nonce journal / AES-XTS — design change) |
| H4 | `crates/euroagent/src/json.rs` | Unbounded recursive descent → stack-overflow DoS, reachable from MCP/AF_UNIX + LLM responses. | ✅ depth limit |
| H5 | `crates/eurowasm/src/lib.rs` | Out-of-range local/func/type indices panic the interpreter (guest-triggered kernel DoS). | ✅ bounds-checked |
| H6 | `crates/eurowasm/src/lib.rs` `MemoryGrow` | Unbounded `mem.resize`, no max-pages, overflow in `delta*PAGE`; `manifest.max_memory_mb` never enforced. | ✅ clamped |
| H7 | `kernel/src/wagent.rs` | Host import reads `args[0]/args[1]` without checking `args.len()` → guest-triggered panic. | ✅ |
| H8 | `crates/eurofs/src/disk.rs:743` | Snapshot label sliced at byte 28 → panic on a multibyte UTF-8 boundary. | ✅ |
| H9 | `crates/eurofs/src/disk.rs` `load_objmap` | Trusts on-disk `count` (objmap not checksummed) → panic on corrupt metadata. | ✅ bound |
| H10 | `kernel/src/gpt.rs` | GPT header/array CRC never verified on read → trusts a corrupt partition table. | ✅ |
| H11 | `kernel/src/ring3.rs` `load_elf64` | `p_offset+p_filesz` / `p_vaddr+p_memsz` bound checks overflow on crafted u64 headers; `rd_u*` unchecked indexing panics. | ✅ checked_add + bounds |
| H12 | `crates/eurocoreutils/src/find.rs` `glob_bytes` | Doubly-recursive `*` matcher → exponential backtracking (ReDoS) on `-name`. | ✅ linear matcher |
| H13 | `crates/eurocalc/src/lib.rs` | Unbounded recursive formula parser → stack overflow on deep nesting. | ✅ depth limit |

## MEDIUM / LOW (documented)

| # | Where | Issue | Status |
|---|-------|-------|--------|
| M1 | `crates/eurovault` `seal` / `kernel/src/vault.rs` | Caller-supplied seal nonce, reused (hardcoded `[0x11;12]` in selftest); ChaCha20-Poly1305 nonce-reuse risk. | ✅ counter nonce |
| M2 | `crates/eurocalc` `%` | No divide-by-zero guard (returns NaN, unlike `/`). | ✅ |
| M3 | `crates/eurocoreutils/src/compute.rs` `expr` | Unchecked `i64` arithmetic → debug overflow panic. | ✅ saturating |
| M4 | `crates/eurowifi` `prf` | `u8` counter overflows for `out_len > 8160`. | ✅ guard |
| M5 | `crates/eurodocio/src/xml.rs:37` | A trailing bare `<` indexes `b[i+1]` OOB → panic on malformed OOXML/ODF. | ✅ |
| M6 | `crates/eurolocale/src/number.rs` | `10u128.pow(frac_digits)` overflows for `frac_digits ≥ 39`. | ✅ clamp |
| M7 | `kernel/src/ring3.rs` `sys_sbrk` | `old + a1` can overflow the `> HEAP_END` guard. | ✅ checked_add |
| M8 | `kernel/src/swapmgr.rs` | Slot encoded `<<12`, decoded `&0xFFFFF` (20-bit) — truncation if slots ≥ 2²⁰. | 🔭 assert (bounded today) |
| M9 | `crates/euroagent/src/json.rs` | `push(c as char)` mangles multi-byte UTF-8; `\u` surrogate pairs not combined. | 🔭 (correctness, not safety) |
| L1 | `kernel/src/sched.rs` | `THREAD_KSTACK_NEXT` never reclaimed → 8-clone cap is permanent (slot leak). | 🔭 |
| L2 | `crates/euroagent/src/manifest.rs` | `tools.denied`/`network_domains` parsed but never enforced (caps still bound). | 🔭 defence-in-depth |
| L3 | `crates/eurotls/src/handshake.rs:319` | Non-constant-time Finished-MAC compare (single-shot, low risk; no `subtle` in tree). | 🔭 |
| L4 | `crates/eurofs/src/cache.rs` | Write-through cache never sets `dirty` → write-back machinery is dead code (trap if flipped). | 🔭 |

---

## Audited clean (verified sound — no defect)

- **TLS 1.3** key schedule (RFC 8446 HKDF-Expand-Label + transcript, test-vector match), AEAD per-record nonce (IV⊕seq, monotonic), signature verification (ECDSA P-256/384, Ed25519, RSA PKCS#1 w/ strict DigestInfo, RSA-PSS EMSA-PSS), the **X.509 DER parser** (rejects indefinite/non-minimal lengths, checked arithmetic, never panics on garbage), and **chain validation** (RFC 5280 §6.1-lite: per-step sig, name chaining, `basicConstraints CA:TRUE`, validity, SAN).
- **EuroCA / EuroAttest / EuroIDM / bundle** TBS encodings — all domain-separated + length-prefixed; no length-extension or type-confusion; tamper/escalation tests hold.
- **EuroVPN** quadruple-DH key separation (ee/es/se/ss) + directional HKDF labels + mutual auth (only the transport-replay gap, H2).
- **EuroWiFi** WPA PTK (correct hand-rolled HMAC-SHA256 + IEEE PRF min/max ordering).
- **Capability model**: `policy::derive` `(required∪granted)∩user −denied`, the MCP cap-gate (every call, fail-closed, audited), EuroPol deny-wins, EuroVault all-bits gate, registry publisher-pinning.
- **Storage**: A/B-superblock torn-write target selection (never overwrites the sole valid slot), CoW commit ordering, FrameAllocator double-free detection, BadBlockTable, SwapArea/Clock, inode + per-data XXH3 bit-rot detection.
- **Kernel**: GDT/TSS + SYSCALL selectors, IDT + IST + ring-3-fault recovery, context-switch + stack canaries/guard pages, SMP bring-up + per-CPU queues, W^X/NX, EuroTPM command framing, AF_UNIX switchboard (no slot reuse → no UAF), EuroFW first-match + CIDR.
- **eurogpu, euroaccess, euroinstall, eurorepro, europkg** parsers/logic — bounds-guarded, no attacker-reachable panic.

The cryptographic and structural foundations are solid; the defects are concentrated in **(a)** the
syscall/driver boundary trusting hardware/ring-3 input, **(b)** the WASM interpreter & JSON parser
trusting untrusted modules/messages, and **(c)** parser robustness on malformed input — all addressed
in the fix pass below (✅), with three design-level items (H3 FDE, M9 UTF-8, L-tier) tracked for a
focused hardening sprint.
