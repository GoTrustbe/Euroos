# EuroOS × Zero Trust for AI Agents

*How EuroOS and EuroAgent map onto the emerging Zero Trust framework for autonomous AI agents (Anthropic, "Zero Trust for AI Agents", building on NIST SP 800-207, NSA Zero-Trust Implementation Guides, and OWASP "least agency").*

**Why this matters for messaging.** Zero Trust is no longer a buzzword — it is codified guidance (NIST SP 800-207, 2020; NSA ZIGs, 2026), mandated for all US federal agencies by 2027, and now being extended specifically to agentic AI. The framework's three principles are *never trust / always verify*, *assume breach*, and *least privilege* (extended by OWASP to **least agency** — restricting what each agent *tool* can do, how often, and where). EuroOS was architected around exactly these principles from the first line of kernel code, which lets us make a rare claim honestly: **most of this framework is not something we bolt on — it is what the operating system already is.**

---

## The framework's own design test — and why capabilities pass it

The whitepaper gives a single test for any control: **"does this make the attack impossible, or just tedious?"** Controls whose value is friction (extra hops, rate limits, non-standard ports, SMS MFA) "degrade significantly against an adversary that can grind through tedious steps at scale." The controls that survive are "hardware-bound credentials, expiring tokens, cryptographic identity, and **network paths that do not exist rather than paths that are merely inconvenient**… prefer a control that removes a capability over a control that throttles it."

This is the definition of a capability model. In EuroOS a process (or an agent) that lacks a capability does not face a slower path to the resource — **the path does not exist**. The syscall returns `-EPERM` before doing anything; the MCP tool is not even listed for that agent; the file outside the sandbox cannot be named. EuroOS is, structurally, a system of "removed capabilities, not throttled ones."

The second load-bearing principle is **assume breach → contain blast radius**. EuroOS does not claim to stop prompt injection (no system reliably can — Microsoft Research is cited that LLMs cannot distinguish instructions from data). Instead, a *successfully* injected agent is contained: it can only ever act within the capabilities it holds, every action is audited, and it can reach nothing its manifest did not declare. That is the whole point of the capability model — designed for breach from day one.

---

## Control-by-control mapping

Legend: **✅ native** (built into the OS, verified) · **🟡 partial** (foundation present, packaged product on the roadmap) · **⬜ gap** (not in scope today, honest).

### 1. Agent identity & authentication

| Framework control (tier) | EuroOS / EuroAgent | Status |
|---|---|---|
| Unique **cryptographic** identifiers per agent (Foundation) | Each agent ships as an Ed25519-signed `.euroa` bundle; the signature is verified **before** the manifest is parsed (no trust before verification). The registry binds an agent name to a publisher key. | ✅ |
| Certificate / signature-based auth, lifecycle (Enterprise) | EuroIDM issues Ed25519-signed, expiring, OIDC-like tokens (subject + groups + validity) that any service verifies locally; appending a group after signing invalidates the signature. EuroCA is a sovereign local CA (issue / verify / revoke). | ✅ / 🟡 |
| **Hardware-bound** credentials, attested issuance, TPM/HSM (Advanced) | EuroTPM (measured boot, hardware RNG, PCR extend); EuroVault master key drawn from the TPM; EuroFDE disk key TPM-sourced. EuroAttest produces signed PCR quotes for remote attestation. | ✅ foundation / 🟡 (full PCR-*sealing* of keys is the documented next mile) |
| Anti–rug-pull / publisher hijack | The registry refuses to install an agent whose name already exists under a *different* publisher key (`PublisherMismatch`) — a second publisher cannot silently replace a trusted agent. | ✅ |

### 2. Access control & privilege management (Least Agency)

| Framework control (tier) | EuroOS / EuroAgent | Status |
|---|---|---|
| RBAC with **deny-by-default** (Foundation) | EuroGuard: every sensitive syscall requires a capability; anything not explicitly granted is denied. Capabilities can be **dropped but never regained**. | ✅ |
| Context-aware / attribute-based policy (Enterprise) | EuroPol compiles declarative policy into a capability mask where **deny always wins**; it can only *reduce* an agent's set, never add. | ✅ (path/cap rules) / 🟡 (time/risk attributes) |
| **Continuous** authorization, re-evaluated per action (Advanced) | EuroGuard checks the capability on **every** syscall — authorization is per-action at the kernel boundary, not per-session. | ✅ at syscall level / 🟡 (dynamic session-level revocation on risk signals) |
| **Least agency** — restrict what each *tool* can do | EuroAgent's MCP gateway gates every tool by a required capability (`ERR_CAP_DENIED` = -32001) and exposes only the tools the cap-set may call. The 3-stage derivation is `required ∪ (optional ∩ granted)`, clamped to the user's caps, minus EuroPol denials. | ✅ |
| JIT/JEA, scoped, auto-expiring elevation (Advanced) | EuroIDM tokens carry expiry; per-session caps are fixed for the session. | 🟡 (JIT elevation + auto-revoke-on-completion is roadmap) |

### 3. Resource boundaries & isolation

| Framework control (tier) | EuroOS / EuroAgent | Status |
|---|---|---|
| **Identity-based isolation** — "services accept connections only from explicitly named callers; network segmentation is a backstop, not the boundary" (Foundation) | The MCP gateway refuses any tool the caller's caps don't name; EuroIPC tags every message with the sender's app identity; the FS tool backend confines an agent to `/agents/<name>/` (no path escape). The boundary is identity/capability, exactly as recommended. | ✅ |
| **Sandboxed execution per agent**, syscall filtering, gVisor/containers (Enterprise) | Agents run as **capability-isolated WASM modules** in EuroOS's own interpreter; host imports are gated per capability and per container (`eurosandbox`). Where the framework reaches for gVisor/containers on top of Linux, EuroOS makes the sandbox the OS itself. | ✅ |
| **Hardware isolation** / confidential computing (SEV/TDX), microVM, "not even the host OS can inspect the workload" (Advanced) | EuroOS's isolation is the capability kernel + WASM, not a hardware enclave hiding from an untrusted host. Different model: there is no large untrusted host OS to hide from — the OS is the TCB. | ⬜ (no SEV/TDX enclaves) — by design a different approach |

### 4. Observability, auditing & traceability

| Framework control (tier) | EuroOS / EuroAgent | Status |
|---|---|---|
| Comprehensive action logs with identity + context (Foundation) | Every MCP tool call is recorded (allowed/denied) with the agent's identity; the agent-loop keeps a per-step transcript. | ✅ |
| **Immutable audit trails with integrity verification**, append-only (Enterprise) | Two complementary mechanisms: EuroID's **SHA-256 hash-chain** audit log (editing any past record breaks every later hash; `verify-chain` + root hash) and the P3 **append-only** log enforced by EuroFS's `FLAG_APPEND_ONLY` (the filesystem physically rejects rewrites; clearing the flag needs `CAP_IMMUTABLE_ADMIN`). | ✅ |
| Traceability / request IDs / provenance chains (Foundation→Advanced) | Audit events carry actor identity and event sequence; the agent transcript links tool calls to the triggering intent. | 🟡 (full OpenTelemetry-style distributed tracing across multi-agent flows not implemented) |
| Real-time SIEM streaming + correlation (Advanced) | EuroObserve renders Prometheus/OpenMetrics; a `/metrics` HTTP endpoint and live SIEM streaming are roadmap. | 🟡 |
| Behavioral baselines / anomaly detection / automated response (Enterprise→Advanced) | EuroObserve provides metrics, but agent behavioral baselining, drift detection, and automated containment are **not** implemented. | ⬜ honest gap |

### 5. Supply chain & memory hygiene

| Framework threat | EuroOS / EuroAgent | Status |
|---|---|---|
| **Tool poisoning / MCP rug pull / falsified tool metadata** | Tools are kernel-defined with a fixed required-capability; agents cannot smuggle hidden capabilities through metadata. Agent bundles are Ed25519-signed and verified before use. | ✅ |
| **Tool-chaining / confused-deputy** ("combine a CRM tool + email tool to exfiltrate") | Even chained tools are bounded by the agent's capability set and EuroPol denials; an agent without `NET`/`vault` caps cannot exfiltrate regardless of which tools it chains. (Containment, not prevention of the request.) | ✅ contain |
| **Memory-based privilege retention** ("agent caches credentials across sessions") | Credentials live in EuroVault, capability-gated, and are not held in agent memory across sessions; capabilities do not persist beyond their grant. | ✅ |
| **Software supply-chain / dependency sprawl / 100 malicious models** | EuroOS is from-scratch `no_std` Rust with a deliberately minimal dependency tree — it does not dynamically load sprawling third-party packages. `.eupkg` packages are SHA-256 + Ed25519-signed and verified before install; **EuroRepro** gives reproducible-build attestation with multi-builder consensus (source→binary→signed-image verifiability). | ✅ |
| **Model supply chain** (poisoned weights, 250-doc backdoor) | EuroOS runs a **local, sovereign** model (Ollama-compatible) you control, rather than an opaque hosted endpoint — but model-weight provenance/scanning itself is the operator's responsibility. | 🟡 (local control helps; weight attestation is out of scope) |

### 6. Input/output controls

| Framework control | EuroOS / EuroAgent | Status |
|---|---|---|
| Input sanitization / prompt-injection filtering | Not done at the OS layer (a model-layer concern). EuroOS's stance is the framework's own "assume breach": it **contains** a compromised agent rather than trying to perfectly filter inputs. | ⬜ by design (contain, don't claim to prevent) |
| Output controls / data-loss constraints | Enforced as capabilities: an agent with no `NET`/`vault`/global-FS capability has no egress path to leak through, whatever it is tricked into producing. | ✅ contain |

---

## Where EuroOS is unusually strong vs. the tiers

Several controls the framework places at **Advanced** (i.e. "aspirational for most organizations") are **Foundation-level / native** in EuroOS, because they are properties of the OS rather than add-ons:

- **Identity-based isolation** — native (capability + identity at every boundary), where the framework treats per-workload cryptographic identity as something to engineer on top of Kubernetes.
- **Hardware-rooted credentials & attestation** — TPM measured boot, sealed vault, signed PCR quotes are built in.
- **Immutable, integrity-verified audit** — a cryptographic hash-chain *plus* filesystem-enforced append-only, not a logging add-on.
- **"Impossible, not tedious"** — the capability model removes paths by construction; this is the framework's gold-standard control pattern, native here.

## Where we are honestly partial or out of scope

- Behavioral monitoring / anomaly detection / ML baselines / automated response (no agent-behavior analytics).
- Distributed tracing / OpenTelemetry across multi-agent workflows; live SIEM streaming.
- Hardware confidential-compute enclaves (SEV/TDX) — EuroOS uses a different isolation model (the OS *is* the TCB).
- mTLS-with-pinning as the agent transport (local MCP runs over AF_UNIX today); JIT privilege elevation with auto-revoke; full PCR-*sealing* of vault/disk keys.
- Model-weight provenance/scanning (operator responsibility; EuroOS gives you a local model you control).

---

## One-line positioning

> **EuroOS is Zero Trust for AI agents, enforced at the operating-system level.** Where the industry layers least-agency, identity-based isolation, hardware-bound credentials and immutable audit on top of Linux and Kubernetes, EuroOS makes them native kernel primitives — capabilities that remove the path rather than throttle it, on a sovereign, offline, EU-built OS designed for breach from the first line of code.

*Sources: Anthropic, "Zero Trust for AI Agents"; NIST SP 800-207 Zero Trust Architecture; NSA Zero-Trust Implementation Guides; OWASP "least agency". Status labels reflect the EuroOS source tree at build 2026.06.08 — see `docs/EUROOS-DEEP-TECHNICAL-REFERENCE.md` for the per-subsystem detail behind each claim.*
