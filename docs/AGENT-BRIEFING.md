# EuroOS — Briefing for an Autonomous Coding Agent

*Read this first. It is the single entry point so you (the agent — written for "Fable") can be productive without spelunking the whole tree. It tells you what EuroOS is, how it's built, what already works and is proven, where we are today, what still has to happen, and exactly where and how to work — including how to turn the Red Hat and Anthropic "Zero Trust for AI agents" guidance into code so the OS actually does what we promise.*

**Repo:** `/home/user/eurokernel` · **Build of record:** `2026.06.08`, alpha · **Scale:** 104 kernel modules + 57 library crates, ~63 000 lines of `no_std` Rust, **690 host tests**, 0 boot panics.

---

## 0. TL;DR — your mission

EuroOS is a from-scratch, sovereign European operating system in `no_std` Rust (x86-64 UEFI), **not** Linux/BSD. Its flagship promise is **"Zero Trust for AI agents, enforced at the OS level"**: AI agents run as capability-isolated WASM modules whose trust boundary is the kernel, with hardware-rooted credentials, identity-based isolation, and an immutable audit trail — sovereign and offline.

A lot of that is **already real and boot-verified**. Some of it is still **scaffolding, stubs, or honest gaps**. Your job is to **close the gap between what we publicly promise and what the code actually delivers**, in priority order, without ever faking it. The backlog in §7 tells you precisely where to work, why, and how to prove it's done.

---

## 1. The hard rules (non-negotiable — read twice)

1. **Never fake as real.** Never present hardcoded, mocked, or demo content as working software. Always label demo vs real. Distinguish *"engine works"* from *"the full app/feature works"*. **Under-claim.** If a path falls back to a mock, the output must say so (e.g. `[mock]`). This rule has bitten us before; it is the most important rule here.
2. **Verify by running.** A change is not "done" until you have (a) `cargo test` green for the affected crate, and (b) where it touches the kernel, a boot self-test marker (`[xx]`) that proves it live. Quote the actual output. If tests fail, say so with the output.
3. **Sovereign by construction.** No dependency on non-EU services. Crypto is from-scratch and pinned to official RFC test vectors, or built on audited RustCrypto primitives — never negotiated down. Data formats are owned (`EUROFS01`, `.euroa`, `.eupkg`).
4. **Capability-native, not Linux.** The native identity is EuroGuard capabilities / EuroIPC / `Euro*`. The Linux syscall ABI is a **compatibility bridge only**, never the identity. Don't "fix" things by making them more Linux-like.
5. **`#![forbid(unsafe_code)]` on library crates.** Keep the host-tested cores safe. `unsafe` lives only where it must (the kernel's hardware layer), and is justified in a comment.
6. **Honesty labels in docs and UI.** Use `✅ native/verified` · `🟡 partial` · `⬜ gap` consistently. The whole project's credibility rests on this.

---

## 2. What EuroOS is (orientation in one screen)

- **Boot:** two-stage A/B UEFI loader → `no_std` kernel that does its own `ExitBootServices`, paging, GDT/IDT/APIC, SMP, scheduler. Identity-maps the lower 512 GiB (1 GiB huge pages) so MMIO + heap are phys=virt → DMA without an IOMMU.
- **Security primitive:** the **capability**. A process holds a `u64` capability mask checked at the syscall boundary; rights are **dropped but never regained**; policy (EuroPol) can only *reduce* the set. "A capability you don't hold isn't a slower path — it's no path."
- **The `Euro*` stack:** EuroFS (CoW filesystem, A/B superblock, snapshots, immutability), EuroNet (own TCP/IP), EuroTLS 1.3, EuroGuard (caps), EuroVault (secrets), EuroID (users/Argon2id), EuroAgent (the AI-agent runtime), EuroTPM (measured boot), and ~50 more — see the glossary in the deep reference.
- **The pattern you'll see everywhere:** the security-critical logic lives in a pure, host-tested `crates/euro*` library (compiles under `std`, `cargo test`); the kernel module is thin glue wiring it onto hardware and printing a `[xx]` boot self-test. **This is why there are 690 host tests *and* a boot self-test for almost everything — replicate this pattern for anything you add.**

You do **not** need to read all the code to understand a subsystem. The authoritative references are:
- **`docs/EUROOS-DEEP-TECHNICAL-REFERENCE.md`** — the deepest per-subsystem description (data structures, algorithms, formats, file:line citations, honest status). **This is your encyclopedia — grep it before reading source.**
- **`docs/ZERO-TRUST-FOR-AI-AGENTS-MAPPING.md`** — how our controls map onto the Zero Trust framework, with the honest gap list (this is the source of much of the backlog).
- `docs/TECHNICAL-OVERVIEW.md` (condensed), `docs/ROADMAP.md` (plan), `docs/SECURITY-AUDIT.md`.

---

## 3. Repo map — where everything lives

```
/home/user/eurokernel
├── kernel/src/*.rs        104 modules: boot (main.rs), paging, sched, ring3 (processes + Linux ABI),
│                          drivers (virtio_*, nvme, xhci, hda, ps2), compositor/desktop, shell.rs,
│                          and one thin module per subsystem (euroid.rs, agent.rs, vault.rs, ...)
│                          + the [xx] boot self-tests (most subsystems' selftest() lives here)
├── crates/euro*/          57 host-tested no_std library cores (the real logic). Examples:
│                          euroid (users/Argon2id), euroagent (agent runtime), eurofs, eurotls,
│                          euronet, eurovault, europol, eurotpm, eurowasm, eurocoreutils, euroweb...
├── loader/                the two-stage UEFI A/B loader
├── toolchain/             eupkg signing keys (dev.pub baked into the kernel), musl test binaries
├── scripts/               build.sh, run-qemu.sh, screenshot.py, release-web.sh, e2e harnesses
├── docs/                  the references above + plan docs
└── Cargo.toml             workspace: `members` + `default-members` (host-test set)
```

**To find anything:** grep `docs/EUROOS-DEEP-TECHNICAL-REFERENCE.md` for the subsystem name → it cites the exact `file:line`. Then open that.

---

## 4. The build / test / run / verify loop (your inner loop)

```bash
# Host tests (fast, no VM) — run these constantly. The sans-IO crate cores test under std.
cargo test                      # whole workspace (690 tests)
cargo test -p euroid            # one crate

# Build the kernel UEFI binary (~18 s) and the bootable image
cargo kbuild-release            # → kernel binary
./scripts/build.sh release      # → eurokernel.img

# Boot headless and read the [xx] self-test markers over serial.
# NOTE: no KVM in this sandbox → TCG is ~60× slower; full boot to the late tests takes ~4-5 min wall-clock.
# Only ONE qemu may hold the image lock at a time (pkill -f eurokernel.img between runs).
timeout 600 qemu-system-x86_64 -machine q35 -m 256M -cpu qemu64,+smep,+smap \
  -bios /usr/share/ovmf/OVMF.fd -drive format=raw,file=eurokernel.img \
  -display none -serial stdio -no-reboot > /tmp/boot.log 2>&1 &
# then: grep -aE '\[k1\]|\[aa\]|panic|MISLUKT' /tmp/boot.log   (MISLUKT = "FAILED" in NL)
```

**Verification discipline (definition of done):**
- Library change → `cargo test -p <crate>` green, with new tests covering the new behaviour (happy path + failure path + a security/edge case).
- Kernel change → a boot run where the relevant `[xx]` line prints `✓` (not `MISLUKT`) and there are **0 panics**.
- Mock-over-SLIRP pattern for anything needing a network peer (Ollama:11434, IPP:631): run a host mock **as a child of the boot task**, boot, and the guest's real request reaches the host — the mock's log proves it. A standalone background mock dies; commands often exit 144 on cleanup (that's pkill noise, not failure).
- Never claim a `[xx]` passed without the line in the log.

---

## 5. Where we are today (the honest snapshot)

Do **not** trust this section over the code — but use it to orient. The authoritative, per-control status is **Appendix B of the deep reference** ("Global honesty matrix") and the **`ZERO-TRUST-FOR-AI-AGENTS-MAPPING.md`** status columns.

**Real & boot-verified (the bulk):** the core kernel (paging/W^X, scheduler, SMP, IDT/APIC, drivers); EuroFS (CoW, A/B superblock, snapshots, scrub, immutability, crash-consistency); ChaCha20 full-disk encryption; A/B atomic updates (Ed25519-verified apply); the TCP/IP path + DNS anti-spoofing; **TLS 1.3** with real X25519/ChaCha20-Poly1305 + cert-chain validation against EU-first roots; the VPN; TPM measured boot; **EuroGuard** capability gating on both ABIs; **Argon2id + Blake2b pinned to RFC vectors**; the SHA-256 audit hash-chain; Ed25519 binary signing with a boot tamper test; the WASM interpreter + cap-gated host imports; the EuroAgent manifest/caps/policy/MCP-gateway/agent-loop/Ed25519-bundle/registry + AF_UNIX MCP daemon; native virtio-GPU 2D scanout; the real HDA audio driver; the accessibility tree; IPP printing; the EuroSuite/EuroWeb/EuroJS engines.

**Scaffolding / stubs / honest gaps (your backlog source):**
- **EuroAgent tool backends:** `fs_*` and `display_notify` are wired to real EuroFS; **`net_get` / `vault_get` / `exec` are NOT wired in-kernel** (`kernel/src/agent.rs:121` returns an error string). The boot/dispatch demos drive the loop with a **scripted mock model** (`ScriptedLlm`, `agent.rs:129`) when no live Ollama is reachable; the real transport (`NetOllama`, `agent.rs:160`) exists but isn't the default proven path.
- **EuroID** store is **in-memory, rebuilt each boot** (no `serialize`/`deserialize` yet; `crates/euroid/src/model.rs`). The desktop/shell login still uses the **legacy SHA-256 `auth.rs`** path, not `euroid::authenticate`.
- **Hardware-rooted trust** is TPM-*sourced* but not PCR-*sealed* (vault + FDE keys); reserved superblock slots are zeroed.
- **Monitoring:** no agent behavioural baselines / anomaly detection; metrics only (EuroObserve). No distributed tracing; no live SIEM stream. No `/metrics` HTTP endpoint.
- **Other breadth gaps:** twelve EuroApps are verified *engines with no GUI window yet*; browser has no image decoding/forms; no intelligible TTS (earcons only); no GPU 3D; coreutils long-tail (xargs, pipe-stdin for some built-ins); installer execution beyond the dry-run; JIT privilege elevation; mTLS-with-pinning as the agent transport.

---

## 6. The engineering north stars (Red Hat + Anthropic → what "delivering on the promise" means)

We now publicly claim Zero Trust for AI agents. Anthropic's *"Zero Trust for AI Agents"* (NIST SP 800-207, NSA ZIGs, OWASP **least agency**) and Red Hat's published agentic-AI security work define what that has to mean in practice. Hold these as design tests for every agent-related change:

1. **"Impossible, not tedious."** Prefer a control that **removes a capability** over one that throttles it. A network path the agent's caps don't grant must not exist — not merely be inconvenient. (We already do this at the syscall edge; keep it true as you wire real tools.)
2. **Assume breach → contain blast radius.** We do **not** claim to prevent prompt injection. We claim a *compromised* agent can still only act within its capabilities, reaches nothing its manifest didn't declare, and has every action audited. Every tool you wire must preserve this.
3. **Least agency.** Scope each *tool*, not just each identity: what it can do, how often, where. The MCP gateway already gates tools by required capability — keep new tools gated and audited.
4. **Identity-based isolation.** Every workload carries its own cryptographic identity; a service accepts only the callers its policy names. Network segmentation is a backstop, not the boundary. (`.euroa` Ed25519 bundles + the cap-gated gateway are the foundation.)
5. **Credentials at the boundary** (Red Hat's OpenShell pattern): inject short-lived, scoped credentials at the tool boundary "so even a compromised agent holds nothing to exfiltrate." → **EuroVault must hand secrets to a tool call, never into the agent/model context.**
6. **Immutable, integrity-verified audit.** Append-only + cryptographic hash-chain. We have both mechanisms; agent tool-calls must land in them and survive reboot.
7. **Hardware-bound credentials + attestation.** TPM/PCR-sealed keys, signed quotes. (We have measured boot + attestation; sealing is the gap.)
8. **Short-lived scoped tokens, JIT/JEA.** Elevate only for the task, auto-revoke on completion.
9. **Supply chain:** signed packages (`.eupkg`), reproducible builds (EuroRepro), minimal deps, a *local* model you control. Keep it that way.

---

## 7. Where to work — the prioritized backlog

This is the heart of the briefing. Work top-down. Each item: **goal · why (which promise/north-star it makes real) · where (files) · approach · done (verification)**. Keep the hard rules — especially: if a backend can't reach a real peer, the path must say `[mock]`, never pretend.

> **✅ STATUS 2026-06-12 — P0–P2 closed (boot-verified):** **P0.3** agent + identity audit persisted to the append-only on-disk log (`[p3-agent]`). **P1.1** EuroID store + Argon2id hashes persist across reboots (`[ae-persist]`); **P1.2** `login`/`su` rewired onto `euroid::login` (Argon2id, not SHA-256 — `[ae]`) + an interactive **GUI lockscreen** gates the desktop (`[ag-lock]`); must-change enforced with self-service `chpasswd` (`[ae-mustchange]`). **P2.1** EuroVault/FDE master PCR-sealed — unseals only on a matching measured boot (`[af-seal]`). **P2.2** JIT capability elevation + auto-revoke (`[af-jit]`). **P2.3** deterministic behavioral anomaly detection over the audit stream (`[af-anom]`). Plus the full code audit closed 100% ([`CODE-AUDIT-2026-06-10.md`](CODE-AUDIT-2026-06-10.md)). **Remaining = P3 breadth** (MCP-daemon as a service, coreutils long-tail, browser/apps depth).

### P0 — Make the EuroAgent runtime actually deliver what we now publicly promise

This is the flagship and the thing the new `/platform/` + `/zero-trust/` pages advertise. Today the agent *chain* is proven but the *tools* and the *model* are partly stubbed. Close that first.

**P0.1 — Wire the real MCP tool backends in-kernel (`net_get`, `vault_get`), keep `exec` denied-by-default.**
- *Why:* least agency + "credentials at the boundary" + "impossible-not-tedious" become real instead of stubbed. This is the single biggest promise-vs-reality gap.
- *Where:* `kernel/src/agent.rs` (`FsToolBackend::execute`, the stub at `:121`), `crates/euroagent/src/mcp.rs` (`ToolBackend`, tool defs `:40`).
- *Approach:* in `FsToolBackend`, implement `net_get` via `net::http_fetch`/`fetch_full` **gated on `CAP_NET`/`NET_GET`** and the manifest's `network_domains` allow-list; implement `vault_get` via `eurovault::Vault::get` **gated on `VAULT_READ`**, returning the secret **only into the tool result for that one call** and **never** logging the value. Leave `exec` returning `ERR_CAP_DENIED` unless a future, sandboxed design is approved. Every call already audits via the gateway — keep that.
- *Done:* extend the `[aa-fs]` self-test (`agent.rs`): `net_get` fetches over EuroNet with the cap (and is denied without, and denied for a domain outside `network_domains`); `vault_get` returns a secret with the cap and `EPERM` without; the audit shows the call but never the secret value. `cargo test -p euroagent` covers the gateway gating. 0 panics.

**P0.2 — Make the real LLM path the default; label the mock honestly.**
- *Why:* "more than cool demos." Right now the demos use `ScriptedLlm` silently; that risks violating Rule 1 if anything reads as real.
- *Where:* `kernel/src/agent.rs` (`NetOllama` `:160`, `default_ollama` `:188`, `run_intent`/dispatch `:204`,`:495`; `ScriptedLlm` `:129`).
- *Approach:* try `NetOllama` (10.0.2.2:11434) first; only fall back to `ScriptedLlm` when unreachable, and when you do, **prefix the transcript/serial line with `[mock]`** and say "no local model reachable." Verify the real path with a host Ollama mock as a child of the boot task (SLIRP pattern).
- *Done:* boot log shows a real round-trip driving a tool call when the mock endpoint is up, and a clearly-labelled `[mock]` line when it isn't. `[bb1]` already proves transport; extend it to drive one tool call end-to-end.

**P0.3 — Persist the agent + identity audit to an append-only, hash-chained file on EuroFS.**
- *Why:* "immutable, integrity-verified audit" that survives reboot — the Enterprise-tier control we advertise.
- *Where:* `crates/euroagent/src/mcp.rs` (`AuditRecord`), `crates/euroid/src/audit.rs` (`AuditLog`, the persistence comment at `:348`), `kernel/src/audit.rs` (P3 append-only `/var/log/euro/`).
- *Approach:* serialize the hash-chain audit lines and append them to `/var/log/euro/audit.log` with `FLAG_APPEND_ONLY`; on boot, load + `verify_chain()`; reject tamper. Reuse the existing P3 append-only mechanism (`kernel/src/audit.rs`).
- *Done:* a `[xx]` self-test writes agent tool-call audit entries, a tamper attempt is refused by the FS, and `verify-chain` passes **after a remount** (cross-boot). Host tests for the chain already exist; add the persistence round-trip.

### P1 — Make EuroID real end-to-end (sovereign identity that actually persists and logs you in)

**P1.1 — Persist the EuroID store to EuroFS.**
- *Why:* "sovereign user management" is currently rebuilt each boot — it doesn't actually remember users.
- *Where:* `crates/euroid/src/model.rs` (`UserDb`/`GroupDb` — add `serialize`/`deserialize`), `kernel/src/euroid.rs` (load/save), files `/etc/euro/{users,groups,shadow}.db` + `policy.toml`.
- *Approach:* add host-tested (de)serialization to the crate (keep it `no_std`, no serde needed — follow the manual encoders used elsewhere, e.g. the audit/superblock style). In the kernel, load on boot and save on mutation; mark `shadow.db` `FLAG_IMMUTABLE` + root-only, `users/groups.db` immutable-by-root.
- *Done:* create a user via `eurousers add`, **reboot**, the user is still present and `eurousers list` shows them; `shadow.db` cannot be written without `CAP_IMMUTABLE_ADMIN`. New host tests for the (de)serialize round-trip.

**P1.2 — Rewire `login`/`su`/`passwd` + the desktop login onto `euroid::authenticate`.**
- *Why:* today the real login path is the legacy SHA-256 `auth.rs`, not the from-scratch Argon2id flow we built and advertise.
- *Where:* `kernel/src/shell.rs` (`login`/`su`/`sudo`/`passwd` handlers), `kernel/src/auth.rs` (session state), the desktop login handler.
- *Approach:* route credential verification through `euroid::authenticate` (Argon2id, timing-attack prevention, lockout, audit events), keep `auth.rs` only as the thin session-state holder (uid/gid/name) or migrate it. Use the **sovereign** Argon2id params for real accounts (the reduced `BOOT_PARAMS` are only for the self-test under TCG).
- *Done:* shell `login alice <pw>` authenticates via Argon2id, 5 wrong attempts lock the account, each attempt audits, and the legacy SHA-256 path is gone or clearly bridged. Boot `[k1]`-style line proves it.

### P2 — Close the Zero Trust gaps we now advertise (so the messaging stays honest)

**P2.1 — PCR-seal the EuroVault master key and the FDE key.**
- *Why:* we say "hardware-rooted credentials"; today the key is TPM-*sourced* (RNG) but not *sealed* to PCRs. This is the difference between "from the TPM" and "bound to a trusted boot state."
- *Where:* `crates/eurotpm` (add seal/unseal), `kernel/src/vault.rs`, the FDE wiring in `kernel/src/main.rs` (`[k3]`).
- *Done:* `[xx]` proves the key unseals only when PCRs match and fails on a wrong PCR state; the reserved `kdf_params`/`wrapped_key` superblock slots are populated.

**P2.2 — JIT capability elevation + auto-revoke for agent tasks.**
- *Why:* "short-lived scoped tokens, JIT/JEA" — elevate only for the task, revoke on completion/timeout.
- *Where:* `crates/euroagent/src/policy.rs` (the `needs_confirmation`/elevated path), EuroGuard session caps.
- *Done:* an elevated tool call grants the cap only for that call, auto-revokes after, and audits the grant + revoke.

**P2.3 — Minimal agent behavioural baseline + anomaly hooks.**
- *Why:* the Foundation-tier monitoring control we list as a gap. Even threshold alerts (tool-call rate, unexpected tool, data volume) move the needle.
- *Where:* `crates/euroobserve` (counters/thresholds) + `crates/euroagent/src/agentloop.rs` (emit per-step metrics).
- *Done:* the agent loop records per-tool counters; a threshold breach raises an audited alert. Host tests for the threshold logic.

### P3 — Breadth (pick up when P0–P2 are honest-green)

EuroApps GUI windows (the twelve engines need `render()` paths); browser image decoding + forms (`crates/euroweb`); installer execution beyond dry-run (`kernel/src/instexec.rs`); coreutils long-tail (xargs, pipe-stdin for more built-ins); intelligible TTS (a real research effort — keep earcons honest until then); GPU work. See `docs/ROADMAP.md` and the plan docs for the full list.

---

## 8. How to add work (so it matches the codebase)

**Adding or extending a host-tested crate** (the canonical pattern):
1. Put the real logic in `crates/euro<x>/src/` as `no_std` + `#![forbid(unsafe_code)]`. Add it to `members` **and** `default-members` in the root `Cargo.toml`.
2. Write host tests in the crate: happy path + failure path + a security/edge case. Anchor crypto to official test vectors.
3. Add a thin kernel module `kernel/src/<x>.rs` that wires it to hardware/state and a `pub fn selftest()` printing one `[xx]` line. Declare `mod <x>;` in `main.rs` and call `selftest()` in the boot sequence near related tests. Add the dep to `kernel/Cargo.toml`.
4. If it has a shell command, add it to the `match` in `kernel/src/shell.rs`.
5. Run `cargo test`, build, boot, confirm the `[xx]` line `✓` with 0 panics. Update `docs/EUROOS-DEEP-TECHNICAL-REFERENCE.md` (the subsystem entry + Appendix B status) and, if status changed, `docs/ROADMAP.md`.

**Definition of done for any task:** code + tests green + boot-verified + docs updated + an honest status label. If you couldn't verify something (e.g. needs real hardware), say so explicitly and label it `🟡`/`⬜` — do not upgrade a status you didn't prove.

---

## 9. Guardrails — what NOT to do

- Don't make EuroOS more Linux-like to "fix" something; the Linux ABI is a bridge, not the goal.
- Don't add a heavy dependency to a `crates/euro*` core; keep the tree minimal and `no_std`.
- Don't weaken crypto params "to make the test pass." Use reduced params **only** in a clearly-commented boot self-test (as `euroid::BOOT_PARAMS` does), never for real accounts/data.
- Don't claim a boot `[xx]` passed without the line in the serial log; don't claim a host test passed without the green output.
- Don't wire a credential or secret into the agent/model context — secrets go to the **tool**, at the boundary, and are never logged.
- Don't silently mock. If a real peer is unreachable, label the fallback `[mock]`.
- Don't touch the public website or push/commit unless explicitly asked.

---

## 10. Pointer index (open these, in this order, only as needed)

| When you need… | Open |
|---|---|
| Deep per-subsystem detail (data structures, algorithms, `file:line`) | `docs/EUROOS-DEEP-TECHNICAL-REFERENCE.md` |
| The Zero Trust control mapping + the honest gap list | `docs/ZERO-TRUST-FOR-AI-AGENTS-MAPPING.md` |
| The condensed overview | `docs/TECHNICAL-OVERVIEW.md` |
| The roadmap / what's planned | `docs/ROADMAP.md`, `docs/PHASE2-PLAN.md`, the `EURO*-PLAN.md` files |
| Known security caveats | `docs/SECURITY-AUDIT.md` |
| The agent runtime design | `docs/EUROAGENT-PLAN.md` + `crates/euroagent/` + `kernel/src/agent.rs` |
| How to build/run/verify | §4 above + `scripts/` |

**Start here:** read §0–§6 of this file, skim Appendix B of the deep reference for the current honest status, then begin at **P0.1**. Work in small, verified steps; keep the status labels honest; quote your test/boot output. That's how this OS gets built.
