# EuroKernel Full Code Audit — 2026-06-10

Method: six parallel subsystem auditors (kernel core, security/crypto, filesystem,
network, EuroAgent, userspace breadth) over the real source, then **every
CRITICAL/HIGH finding re-verified by hand** against the cited lines. Findings the
verification pass refuted are listed under "False positives / over-rated" so they
don't get fixed twice or block a sprint on a non-issue.

Baseline health: `scripts/test.sh` **green** (exit 0) — builds kernel (2.9M) +
loader, host test suite passes, boot screenshot renders. Honesty posture across
userspace apps is **GREEN**: no mock presented as real (Calc/Impress are labeled
stubs; `ScriptedLlm` is a labeled test double; `[mock]` shown when no LLM peer).

---

## Confirmed findings (verified against code)

| # | Sev | Location | Issue |
|---|-----|----------|-------|
| 1 | HIGH | `kernel/src/ring3.rs` pipe paths (≈182, 209, 343) | **Inconsistent user-pointer validation.** `in_user_arena()` exists and is used by vfs read/write (lines 399, 449) but is *missing* on `pipe_create` (writes 2 fds to `user_fds`), `pipe_read_fd` (`copy_nonoverlapping` into `buf`), and several Linux-ABI handlers (writev iov, clone ctid, execve argv). A ring3 process can pass a kernel address and the kernel reads/writes it. The SAFETY comments *assert* the pointer is in the user arena but nothing checks it. |
| 2 | HIGH | `kernel/src/net.rs:420` | **Hardcoded TCP ISN `0x1000`** on the client connect path (server-side `accept_from` does use randomness). Combined with sequential ephemeral ports, an off-path attacker can forge RST/data into a client connection. Fix: seed ISN from `rand_u64()`. |
| 3 | HIGH | `kernel/src/auth.rs:14,22` (live path: `shell.rs:115`, `main.rs:505`) | **Login still uses iterated SHA-256 (4096), not Argon2id.** The strong Argon2id lives in `crates/euroid` (host-tested, RFC9106 vectors) but is **not wired into the kernel login path**. This is exactly the AE-sprint TODO ("rewire login onto `euroid::authenticate`"). The docstring is honest about it, but the weak hash is what actually guards `/etc/shadow` today. |
| 4 | MEDIUM | `crates/eurofs/src/disk.rs:343, 398` | **Unchecked extent arithmetic `(phys + k) as usize`** during allocator rebuild. A corrupted/crafted on-disk inode with `phys = u64::MAX` wraps and corrupts the allocation bitmap → silent block reuse / data corruption. Fix: `phys.checked_add(k).ok_or(Corruption)?`. |
| 5 | MEDIUM | `crates/eurofs/src/disk.rs:153, 575` | **Silent extent truncation.** `decode()` drops extents past `MAX_EXTENTS` instead of rejecting; `write_object` casts block count to `u32`. Both lead to silent data loss / orphaned blocks on the (admittedly extreme) large-file or corrupt-inode path. Reject rather than truncate. |
| 6 | MEDIUM | `kernel/src/virtio_net.rs:288` | **RX length from used-ring not bounded against `BUF_SIZE`.** A malicious/buggy virtio device can claim `len > 2048` → slice `total - NET_HDR_LEN` reads out of bounds. Add `if total > BUF_SIZE { break; }`. Requires a hostile hypervisor, hence MEDIUM. |
| 7 | MEDIUM | `crates/euroagent` audit (`mcp.rs`) + `kernel/src/agent.rs` | **Audit trail is RAM-only.** Tool calls are audited in a `Vec` but never persisted; reboot loses the trail. This contradicts the AGENT-BRIEFING "immutable audit that survives reboot" wording. Tracked as P0.3 backlog — promise currently exceeds reality on this one line. |
| 8 | MEDIUM | `crates/eurotls/src/handshake.rs:121,434,467` | **Default-insecure TLS API.** `trust_roots: None` → `validate_chain` and `verify_certificate_verify` return `Ok(())` (accept anything). The live kernel path *does* call `set_trust_anchor` (`net.rs:734`) so production is safe, but the default is a footgun: any new caller that forgets the call silently disables cert validation. Make roots a constructor arg, or default-deny. |
| 9 | LOW | `crates/eurotls/src/sig.rs:84,97,179` | **Non-constant-time `==`** in RSA signature hash comparison. Real, but these compare values derived from *public* data (signature + message), so the timing leak does not enable forgery the way a MAC/password compare would. Tidy with `ct_eq` for hygiene; not the CRITICAL the scanner first rated it. |
| 10 | LOW | `crates/eurofde/src/lib.rs:40` | FDE nonce = salt‖LBA (deterministic). Re-writing the same LBA with new plaintext under ChaCha20 reuses the keystream (two-time pad). Document the constraint or add a per-write counter. |
| 11 | LOW | dev ergonomics | `cargo test --workspace` fails (`E0152` duplicate lang item) because the `kernel`/`loader` no_std bins get pulled into the host test build. Not a product bug — but the canonical command is `scripts/test.sh`; worth a one-line note in AGENT-BRIEFING so nobody "fixes" a phantom failure. |

## False positives / over-rated (do NOT spend a fix on these)

- **DNS compression-pointer infinite loop** (claimed CRITICAL, `euronet/src/dns.rs:79`): **refuted.** `skip_name` *returns* on a pointer (`pos + 2`) rather than following it, and every other iteration advances `pos` by ≥1 under a `pos >= buf.len()` guard, so it terminates. `parse_query_name` rejects pointers outright (`len & 0xC0 != 0 → None`). No loop.
- **TLS cert validation "completely skipped" (CRITICAL):** over-rated — the live path sets the trust anchor; real severity is the footgun default (#8).
- **RSA `==` timing (CRITICAL):** over-rated to LOW (#9), public-data comparison.

---

## Recommended order of work (folds into the active AD–AG plan)

1. **#1 user-pointer validation sweep** — highest leverage: add `in_user_arena()` to
   *every* syscall that dereferences a user pointer (writev/readv iov, clone ptid/ctid,
   execve argv, pipe fds/buf, connect sockaddr). Centralize so the next syscall can't
   forget. This is a true local-privilege/kernel-write primitive.
2. **#3 EuroID end-to-end (Sprint AE)** — wire `euroid::authenticate` into `shell.rs`
   login/su and retire the SHA-256 `auth.rs` hash. Already planned; audit confirms urgency.
3. **#7 audit persistence (P0.3)** — make the EuroAgent claim true: append-only hash-chain
   to `/var/log/euro/audit.log`, verify chain on boot.
4. **#2 random TCP ISN**, **#6 virtio RX bound**, **#4/#5 eurofs checked arithmetic** —
   small, self-contained hardening, each with a host test + boot marker.
