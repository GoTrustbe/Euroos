# EuroOS — Sprint Plan AH ("From installable to self-hosting")

*Successor to the AD–AG cycle ("Promise = Reality", now ✅ — see [`SPRINT-PLAN-AD-AG.md`](SPRINT-PLAN-AD-AG.md)). AD–AF closed the security/identity gaps; AG delivered visible breadth (desktop apps, web media/forms, a **sovereign installer**, coreutils). This cycle turns the brand-new installer foundation into a system that installs **interactively**, **updates itself** safely, runs **real sandboxed WASM agents**, and reaches **broader hardware** — completing the highest-value 🟡 "core done" items along the way.*

**Conventions.** Estimates in **sessions**. Kind: `N 🔒` = new/security-critical · `R` = rewire/refactor · `U` = extension · `🏗️` = large/multi-component. Definition of done is unchanged from the AD–AG briefing: **host-tested core → thin kernel glue → `[xx]` boot self-test → docs/status updated → honest status label**. The hard rules hold — **never fake-as-real**, label any `[mock]`.

**Why this order.** AG-3 just shipped `eurofat` (a from-scratch FAT32 ESP writer) + `instexec` (reads the running kernel's own boot media via UEFI and writes a bootable GPT/ESP/EuroFS to a target disk). AH-1 and AH-2 reuse that machinery directly — so they are cheap *and* high-value. AH-3/AH-4 complete the agent-first and Linux-compat promises.

```
AH-1 Interactive install ──┐  (reuses AG-3 instexec/eurofat)
AH-2 A/B self-update     ──┤
                           ├─→ a system you can install, update & roll back
AH-3 Real WASM agents (H4) ┘  (completes the agent-first promise)
AH-4 Breadth: dynamic linker (H3) / hardware (USB-install, WiFi)
```

---

## Sprint AH-1 — Interactive, user-driven install `U 🔒`

**Goal.** Move the install from an automatic boot-time action to something the **user triggers** — from the desktop and from the shell — choosing the target disk. The execution engine (`instexec::install_to_disk`) already works and is boot-verified; this sprint is the front-end + safety rails.

- **What:** a `euroinstall --to <diskN>` shell command and a working **EuroInstall** desktop button that calls `instexec::install_to_disk(dev)` on the chosen virtio target; a target picker (enumerate `virtio_blk` devices + capacity); an explicit "this will erase the disk" confirmation; provision the chosen locale/keymap/hostname/user into the new EuroFS root (the `euroinstall` planner steps, already host-tested).
- **Files:** `kernel/src/installer.rs` (GUI → action), `kernel/src/instexec.rs`, `kernel/src/shell.rs`, `crates/euroinstall`.
- **Done:** in the multidisk harness, the desktop "Install" flow (or `euroinstall --to disk0`) writes a bootable disk that boots standalone **with the chosen hostname/user present** after first boot; refuses a too-small or non-existent target with a clear message; `[q1x3]` self-test drives the user-path end-to-end.

## Sprint AH-2 — Real A/B self-update (finish G4) `N 🔒`

**Goal.** The OS updates **itself**: write a new (signed) kernel image into the **B slot's ESP**, flip the A/B `slot_config`, and boot the new slot on next start — with automatic rollback if the new slot fails to come up. This closes the 🟡 on atomic updates and builds straight on AG-3's disk-writing.

- **What:** extend the `eurofat`/`instexec` path to write a kernel image into the **inactive** slot's ESP (`\EFI\BOOT\eurokernel-B.efi`) without disturbing slot A; verify the image's **Ed25519 signature** before staging (reuse `crypto.rs`); update `/boot/slot_config` (the raw-LBA G4 mechanism in `update.rs`) to point at B and set "pending"; mark the slot "good" only after a successful boot, else roll back to A.
- **Files:** `kernel/src/update.rs`, `kernel/src/instexec.rs`, `crates/euroupdate`, `crates/eurofat`, loader (`loader/src/main.rs` already selects the slot).
- **Done:** `[upd2]` self-test (or multidisk harness): stage a new B image → reboot → loader boots **slot B** → mark-good; stage a deliberately-corrupt B → reboot → boot **falls back to A** (rollback). Signature check rejects an unsigned/tampered image.

## Sprint AH-3 — Real sandboxed WASM agents (finish H4) `N 🔒 🏗️`

**Goal.** Make "EuroAgent agents are WASM modules" literally true: load and run an actual `.wasm` agent module under the kernel's capability sandbox, with WASI-style host calls **gated by the agent's manifest capabilities** (the same gate AD-1 built for MCP tools).

- **What:** complete the `eurowasm` runtime's execution path (the core is 🟡 host-done); a minimal **WASI** surface (args/env, a write-only `fd_write` to the agent transcript, a clock read) where every host call is capability-checked; run a real agent `.wasm` that performs a tool call through the MCP gateway and returns a result.
- **Files:** `crates/eurowasm`, `kernel/src/agent.rs`, `crates/euroagent`.
- **Done:** `[wasm2]` boot self-test: a real `.wasm` agent module runs, a denied WASI/host call is refused by the manifest gate, an allowed one succeeds, and the run is audited. Host tests for the interpreter additions. (If full WASI is out of scope this cycle, ship a labelled subset honestly and stage the rest.)

## Sprint AH-4 — Breadth: run more software / broader hardware `U 🏗️`

Pick per session; independent.

| Item | What | Where | Done |
|---|---|---|---|
| **AH-4a — `ld.so` dynamic linker (finish H3)** | resolve + relocate dynamically-linked musl ELFs so unmodified dynamic Linux binaries run (the core is 🟡) | `kernel/src/{elf,ring3}.rs` | a dynamically-linked `/bin` program runs to completion; `[ld]` self-test |
| **AH-4b — Install to USB mass storage** | use the existing xHCI USB-storage path as an install **target** (not just root) so EuroOS installs to a real USB stick | `kernel/src/{usb,instexec}.rs` | install to a USB-backed virtio/xHCI disk, boot it standalone |
| **AH-4c — WiFi association (finish N1)** | complete the `eurowifi` stack from scan → real association + DHCP over a virtual/HW adapter | `crates/eurowifi`, `kernel/src/net.rs` | associate + obtain a lease; honestly labelled if HW-attended |

---

## Beyond AH (the longer tail — `docs/ROADMAP.md`)

- **Reliability/scale (Sprint J remainder):** per-subsystem lock-ordering audit, NVMe robustness under load, swap pressure tests.
- **Hardware breadth (Sprint I remainder):** real audio output path (intelligible TTS is still the honest "next mile"), printer/scanner, ACPI power states (S3/S5).
- **Big product tracks:** AB **EuroBrowser** own engine (JS, more CSS — AG-2 advanced layout/paint), AC **EuroApps** (more of the 20 apps now that the GUI pattern is proven), **EuroSuite** office polish (real editing, not just rendering).
- **Deliberately deferred:** mTLS-pinned agent transport, distributed tracing/SIEM streaming, SEV/TDX confidential VMs, 3D GPU.

*Source: the AD–AG definition of done + `docs/ROADMAP.md` (the 🟡 "core done" list) + the AG-3 installer foundation (`crates/eurofat`, `kernel/src/instexec.rs`). Sprint codes AH follow AG in `NEXT-SPRINTS.md`.*
