# EuroOS — Forward Sprint Plan (after AG / AH)

*Consolidates what's left into a prioritized, theme-based plan. The "Promise = Reality"
cycle (AD–AG) and the install/update/WASM cycle (AH-1/2/3) are ✅ done and boot-verified;
AH-4 (TLS) was banked. This plan supersedes the open items in
[`NEXT-SPRINTS.md`](../NEXT-SPRINTS.md) (the G–Q board) and groups the `docs/ROADMAP.md`
🟡 / ⬜ remainders by value. Updated 2026-06-13 · **717 host tests green.***

**Where we are.** A from-scratch sovereign OS that boots to a desktop, installs itself to
a disk (and boots that disk standalone), updates itself across A/B slots, runs WASM agents
capability-isolated, renders web pages with images + forms, has three real desktop apps,
and an open audited security/identity stack. The next leap is: **run real Linux software,
boot on real hardware, finish the secure-update story, and become pleasant to use daily.**

Conventions: `🔒` security-critical · `🏗️` large/multi-component · `🧪` host-testable.
Definition of done is unchanged: host-tested core → thin kernel glue → `[xx]` boot
self-test → docs/status updated → honest label. **Never fake-as-real.**

---

> **Progress 2026-06-13:** Sprint 1 **TLS done + boot-verified** (`[tls]`/`[tls2]`: static + cross-module IE-TLS via the kernel-ld.so) — ifunc + the busybox syscall-breadth remain (larger). Sprint 2 **ACPI clean shutdown done + boot-verified** (`[i3-s5]` + `shutdown-test.py`: QEMU guest-shutdown) — WiFi / printer / USB-install / S3-sleep are hardware-attended and not truthfully verifiable in the TCG/QEMU sandbox.
>
> **Sprint 3 done + boot-verified (2026-06-13):**
> - **Loader-owned try-counter + auto-rollback** — the UEFI loader now runs `on_boot()` (decrement / roll back to the last-good slot) and writes `\slot_config` back to the ESP *before* starting the kernel; the kernel can never brick the machine even if it fails to boot. Verified: `update-test.py` RUN3 shows `[loader] on_boot: B tries 3 → 2` then boots slot B. The full counter→exhaust→auto-rollback→mark-good cycle is proven on the **real on-disk ESP** via a new memory-light sectored FAT32 read/modify/write (`eurofat::sectored`, host-tested) → `[upd4] … OK ✓`.
> - **Ed25519 verify-before-activate** with a genuine `dev.key`-signed test image (committed artifact, verifies against the embedded `dev.pub`): `[upd3]` proves a real signature is accepted and any image/sig/length tampering is refused, end-to-end through the update pipeline.
> - **`euroupdate fetch <url>`** wired over the real EuroTLS-1.3 stack (`net::post_full`/`fetch_full`) → verify → stage B, honestly reporting the sandbox's lack of external network.
> - **User-facing immutability admin tool** (`euroimmutable status|list|lock|unlock`) over the existing `CAP_IMMUTABLE_ADMIN`-gated L1/L2 (`[l1]`).
>
> **Sprint 4 done + boot-verified (2026-06-13):**
> - **3 new dock apps with live data** (dock 8→11 tiles): **EuroText** (real plain-text editor, edits + saves to EuroFS), **EuroMonitor** (live RAM/tasks/disk/audit), **EuroLog** (live hash-chained audit log).
> - **Document editing**: `[edit]` proves type → save → reload round-trips on the real EuroFS.
> - **Web form POST**: the EuroWeb engine builds a correct `method="post"` request (urlencoded body) over the same TLS/TCP stack as GET → `[post]`.
> - **JS on a page**: EuroJS executes page `<script>`s once on load; `console.log` is captured and `document.write` mutates the rendered DOM → `[js]`.
> - **Remaining (honest):** rich EuroSuite Writer/Calc *editing* (EuroText covers plain-text editing); a real mid-line cursor/arrows needs an extended PS/2 driver; full DOM scripting (`getElementById`, events) beyond `document.write`/`console.log`.

## Sprint 1 — Run real Linux software (finish the dynamic linker) `🏗️` *(= the banked AH-4)*
**Goal.** Run unmodified, dynamically-linked Linux binaries (busybox, curl) against a real
`libc.so` — the single biggest unlock for the app surface.
- **TLS relocations** (`R_X86_64_TPOFF64`/`DTPMOD64`/`DTPOFF64`): set up the static TLS
  block + `fs_base` at link time (the per-task `fs_base` infra already exists), patch the
  GOT (`tpoff = st_value − tls_size + addend`). Needs a `.so`-provided `__thread` (ld
  relaxes IE→LE for a single PIE), so build `libtls.so` + `dyntls.elf`.
- **ifunc** (`R_X86_64_IRELATIVE`): the resolver is code that must run in **ring3** (SMEP
  forbids ring0 from executing user pages) → a tiny userspace resolver stub before `_start`.
- **Breadth pass:** musl `libc.so` surfaces more syscalls + symbol versioning; iterate.
- **Done:** a dynamically-linked `__thread`-using binary runs (`[ld-tls]`); then a real
  busybox applet runs. Files: `kernel/src/ring3.rs`, `userland/`. *(Analysis: ROADMAP H3.)*

## Sprint 2 — Real-hardware readiness (bare metal) `🏗️🔒`
**Goal.** EuroOS installs (AG-3/AH-1) and boots in QEMU; the next proof is a **real laptop**.
- **N1 — WiFi** (`🏗️🔒`): finish `eurowifi` scan → real association + DHCP over a HW/virtual
  adapter (honestly labelled if HW-attended).
- **I3 — ACPI power**: clean **S5 shutdown** + **S3 sleep** via the AML interpreter.
- **I4 — printer/scanner**: IPP core exists; wire an end-to-end print job.
- **Install-to-USB**: use the xHCI mass-storage path as an AG-3 install *target*.
- **Done:** boot from a USB-installed disk on real hardware, get on WiFi, clean shutdown.

## Sprint 3 — Complete the secure self-update + immutability story `🔒`
**Goal.** A sovereign OS must update itself **securely** and resist tampering. AH-2 built the
A/B rails; finish them and make the system image immutable.
- **G4 finish:** **Ed25519-verify** the staged B image before activating; **loader
  try-counter auto-rollback** (boot B → if it doesn't mark-good in N tries, revert to A).
- **K2 — EuroUpdate delivery:** fetch a **signed update package** over TLS, verify, and
  stage it into the B slot (reuse `eurofat` + `instexec` from AH-2).
- **L3/L4:** immutable system-image partitions + a user-facing immutability API.
- **Done:** `euroupdate fetch <url>` → verify → stage B → reboot → boot B → mark-good, and a
  tampered package is refused (`[upd3]`).

## Sprint 4 — Apps & usability (daily driver) `🏗️`
**Goal.** The platform is proven; now make it pleasant. Build on the AG-1 app pattern + the
AG-2 web engine.
- **AC — EuroApps breadth:** more dock apps on the proven `SuiteApp` pattern (EuroMail,
  EuroSettings depth, EuroPhotos/EuroShot, a calculator window) showing real engine data.
- **EuroSuite editing:** real document **editing** (cursor, input, save), not just rendering.
- **AB — EuroBrowser depth:** form **POST**, more CSS, and basic **JS** via `eurojs`.
- **Done:** open ≥3 more apps from the dock with live data; edit + save a document; submit a
  POST form; run a tiny JS snippet on a page.

## Sprint 5 — Reliability & scale hardening `🔒🧪`
**Goal.** Trust requires it never loses data under stress.
- **J2 — NVMe robustness** under load; **J1 — lock-ordering audit** across subsystems;
  **J3 — swap** pressure tests.
- **EuroFS crash-consistency stress:** A/B-superblock torn-write + power-cut simulation.
- **Done:** fault-injection + stress harness passes with 0 data loss / 0 panics.

---

## Deferred (explicit — not this horizon)
- **H5** — X11/Wayland bridge (EuroXServer) + Flatpak runtime (large; WASM is the preferred
  sovereign app format anyway).
- **Confidential compute** — SEV/TDX VMs · **3D GPU** · **distributed tracing / SIEM
  streaming** · **mTLS-pinned agent transport**.

*Recommended order: Sprint 1 first (biggest unlock + it's already scoped), then 2 or 3 by
appetite (hardware vs. security depth), then 4 (usability) and 5 (hardening) once the base
is broad. Source: `docs/ROADMAP.md` (🟡/⬜ items) + the AG/AH foundations.*
