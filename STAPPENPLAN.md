# EuroKernel — Stappenplan (Build Roadmap)

> Een Europees soeverein besturingssysteem, **from scratch** in Rust.
> Geen Linux, geen BSD, geen bestaande kernel als basis.
> Microkernel · capability-based · UEFI · x86-64 first · nul telemetrie · EUPL 1.2.
>
> Dit stappenplan consolideert de visie (Investor Memorandum) en de 4 technische
> tracks tot één samenhangende bouwvolgorde met afhankelijkheden, mijlpalen en
> concrete eerste-acties.

---

## 0. Bronnen & status

| Document | Inhoud | Status |
|---|---|---|
| Investor Memorandum 2026 | Visie, 5-jaar roadmap, €24M budget, team, architectuur | ✅ verwerkt |
| Track 1 — Claude Code Build Prompt | Bare-minimum bootable UEFI OS (GOP framebuffer) | ✅ verwerkt |
| Track 2 — EuroFS | Eigen filesysteem: ramdisk (Fase 1) → on-disk CoW FS | ✅ verwerkt |
| EuroOS UI-prototype | Visuele huisstijl-referentie voor Track 5 (`design/`) | ✅ verwerkt |
| Track 3 — Kernel Internals | Geheugenbeheer, scheduler, syscalls | ✅ verwerkt |
| Track 4 — EuroNet | Eigen TCP/IP netwerkstack | ✅ verwerkt |
| Track 5 — EuroDesktop | Compositor, UI toolkit, font rendering | ✅ verwerkt |

> **Correctie:** "Track 2" in de documenten = **EuroFS** (filesysteem), niet de
> HAL. De HAL/driver-laag (NVMe/SATA, USB HID, ExitBootServices) zit verspreid in
> het memorandum en heeft (nog) geen eigen track-document.

---

## 0a. Voortgang — geverifieerd op 2026-06-01

Er staat een werkende Cargo-workspace in `/home/user/eurokernel/`:

| Onderdeel | Track | Bewijs |
|---|---|---|
| Bootable UEFI-kernel + GOP-desktop (EuroOS-splash) | 1 | Boot in QEMU+OVMF, `screenshots/boot.png` |
| EuroFS on-disk **CoW**-FS: inodes, extents, dirs, checkpoints, crash-consistent | 2 | 36 host-tests; gemount in kernel, `screenshots/boot-eurofs.png` |
| Interactieve shell (UEFI-toetsenbord): ls/cat/write/mkdir/df/net/mem | 1 | live op EuroFS, `screenshots/shell.png` |
| EuroNet: Ethernet/ARP/IPv4/ICMP/UDP parse+build+checksums | 4 | 13 host-tests; `net`-selftest, `screenshots/net.png` |
| EuroMM: fysieke frame-allocator + UEFI memory-map | 3.1 | 6 host-tests; `mem`-commando, `screenshots/mem.png` |
| **Kernelmodus**: ExitBootServices + eigen GDT/IDT/exceptions + PS/2 + panic + COM1 | 3.2 | interactieve shell zónder UEFI, `screenshots/kernelmode-typed.png` |
| **Preemptief multitasking**: PIT-IRQ + round-robin scheduler + context-switch | 3.3 | 3 taken + shell parallel, `screenshots/sched-typed.png` |
| **IRQ-toetsenbord** + **eigen paging** (4-niveau, eigen CR3) | 3.3/3.4 | `screenshots/irqkbd.png`, `screenshots/paging.png` |
| **Ring-3 userspace + SYSCALL** (privilege-scheiding) | 3.4 | userspace trapt naar kernel, `screenshots/ring3.png` |
| Huisstijl uit UI-prototype (palet + EUROOS-wordmark) | 5 | toegepast op bootscherm |

**Totaal 55 host-tests groen, clippy schoon, kernel boot + screenshot in CI.**
Workspace-crates: `kernel/` (no_std UEFI→kernelmodus) + host-testbare `eurofs`, `euronet`, `euromm`.
De kernel verlaat UEFI (`ExitBootServices`) en draait op eigen heap/GDT/IDT/framebuffer/PS2 +
een preemptieve scheduler (timer-IRQ context-switch tussen kernel-taken).
Volgende: Track 3.4+ — IRQ-toetsenbord, paging, ring-3 userspace + SYSCALL/SYSRET.

Architectuurkeuzes (geen shortcuts): workspace met host-testbare library-crates
(`crates/eurofs`, `no_std`+`alloc`, test onder `std`) + `no_std` kernel-binary;
`cargo test` (tier 1, geen VM) en `cargo kbuild` (UEFI, build-std via alias) zijn
gescheiden; CI draait tier 1 + headless QEMU-boot. **Bug gevonden & gefixt:** de
CP437 8x8-font is LSB-left → `bits & (1<<col)`, niet `0x80>>col` (de spec-code
spiegelde elke glyph). Gevonden dankzij de eerste screenshot — precies waarom we testen.

**Toolchain:** uefi 0.33 (moderne API), Rust nightly 1.98, QEMU+OVMF geïnstalleerd.

---

## 1. Architectuur-pijlers (de niet-onderhandelbare keuzes)

Deze keuzes sturen élke latere beslissing. Ze staan vast in het memorandum:

1. **Microkernel**, geen monoliet. Kleine trusted computing base; drivers draaien
   in **userspace**. Een crashende driver crasht het systeem niet.
2. **Capability-based security**. Geen ambient authority — een proces kan alleen
   wat het via een capability expliciet gekregen heeft. (Inspiratie: seL4.)
3. **IPC is het hart van het systeem**. Alle communicatie tussen kernel-services,
   drivers en apps loopt via message-passing IPC. Dit moet snel én veilig zijn.
4. **Rust, `no_std` + `alloc`**, nightly toolchain. Memory safety zonder GC.
5. **UEFI boot, x86-64 eerst**, ARM64 later (jaar 2+).
6. **Vulkan** voor GPU (open Khronos-standaard) + software fallback.
7. **Nul telemetrie**. Geen netwerkaanroep zonder expliciete gebruikerskeuze —
   afdwingbaar omdat de netwerkstack (Track 4) van onszelf is.
8. **EUPL 1.2** licentie, volledig open source, RFC-proces voor architectuur.

**Eigen code, geen ports van kernel-internals.** Wel toegestaan als
gewone dependency (geen kernel-fork): `fontdue` (fonts), `rustls`+`ring` (TLS —
crypto schrijf je niet zelf), `webpki-roots`. Conceptueel mag je RFC-standaarden
volgen (TCP/IP, OpenType, VirtIO-spec) — die zijn publiek domein.

---

## 2. Afhankelijkheidsgraaf van de tracks

```
            ┌──────────────────────────────────────────┐
            │  TRACK 1 — Boot & Framebuffer (fundament) │
            │  UEFI · GOP · 8x8 font · desktop-demo      │
            └───────────────┬──────────────────────────┘
                            │ (ExitBootServices = overgang naar échte kernel)
            ┌───────────────▼──────────────────────────┐
            │  TRACK 2 — HAL, driver-model, storage      │  ⏳ nog toe te voegen
            │  NVMe/SATA · USB HID · PCI · GOP→DRM        │
            └───────────────┬──────────────────────────┘
                            │
            ┌───────────────▼──────────────────────────┐
            │  TRACK 3 — Kernel Internals (kritiek pad)  │
            │  Frame allocator · paging · heap ·         │
            │  GDT/IDT · scheduler · syscalls · IPC      │
            └──────┬──────────────────────────┬─────────┘
                   │                          │
       ┌───────────▼──────────┐   ┌───────────▼──────────────┐
       │ TRACK 4 — EuroNet     │   │ TRACK 5 — EuroDesktop     │
       │ VirtIO · ARP · IPv4 · │   │ Compositor · widgets ·    │
       │ UDP · TCP · DNS · TLS │   │ fonts · Vulkan            │
       └──────────────────────┘   └──────────────────────────┘
            (T4 vereist T3 syscalls)   (T5 vereist T1 GOP + T3 IPC/proc)
```

**Kritiek pad = Track 1 → Track 3.** Zonder geheugenbeheer + scheduler + syscalls
bestaan Track 4 en 5 niet. Track 4 en 5 kunnen daarna **parallel** door aparte
teams gebouwd worden.

---

## 3. Gefaseerd stappenplan

Elke fase eindigt met een **demonstreerbaar** resultaat (de mijlpalen uit het
memorandum). De maand-nummers volgen de 5-jaar roadmap.

### Fase 0 — Fundament & tooling (Maand 1, Q1 2026)
**Doel:** reproduceerbare build + CI vóór er ook maar één kernel-regel staat.

- [ ] Repo opzetten op Europese Gitea/Forgejo (dag 1, soevereiniteitseis).
- [ ] `rust-toolchain.toml` (nightly, target `x86_64-unknown-uefi`, `rust-src`).
- [ ] `.cargo/config.toml` met `build-std = ["core","compiler_builtins","alloc"]`.
- [ ] `Cargo.toml` met `panic = "abort"` in **dev én release**.
- [ ] CI/CD: `cargo build --release` → `.img` bouwen → in QEMU+OVMF booten in
      headless mode, screenshot als artefact. Dit is de regressietest voor álles.
- [ ] Architecture RFC #1 publiceren (de 8 pijlers uit §1).

**Mijlpaal:** geautomatiseerde build & test pipeline live (Q1-deliverable).

### Fase 1 — Bootable prototype = Track 1 (Maand 1–3, Q2 2026)
**Doel:** "EuroKernel v0.1" op het scherm, in QEMU én op fysieke hardware.

Volg **Track 1** exact:
- [ ] `src/main.rs` — UEFI entry, `uefi_services::init()` als **eerste** aanroep.
- [ ] `src/panic.rs` — panic handler (zonder dit compileert `no_std` niet).
- [ ] `src/graphics.rs` — `write_pixel` (`write_volatile`!), `fill_rect`, GOP mode-select.
      **BGR vs RGB**: detecteer `pixel_format()` — meest gemaakte fout.
- [ ] `src/font.rs` — 8x8 bitmap font (IBM CP437 subset).
- [ ] `src/desktop.rs` — achtergrond, taakbalk, gecentreerd logo, welkomstvenster.
- [ ] `build.sh` (mtools, geen root) + `run-qemu.sh` (OVMF-detectie multi-distro).

**Mijlpaal M-Q2:** kernel print 'EuroKernel v0.1' op scherm (memorandum Q2 2026).

> ⚠️ Track 1 blijft bewust **binnen** UEFI Boot Services. De stap naar een échte
> kernel (ExitBootServices, framebuffer-handover) hoort bij Track 2/3.

### Fase 2 — HAL, ExitBootServices & storage = Track 2 (Maand 6–18) ⏳
**Doel:** los van UEFI-runtime; eigen drivers; lezen van schijf.
*(Wordt verfijnd zodra het Track 2-document binnen is. Verwachte inhoud op basis
van het memorandum:)*

- [ ] UEFI **memory map ophalen** → dan pas `exit_boot_services()` (volgorde is
      cruciaal — na ExitBootServices bestaat UEFI niet meer).
- [ ] Framebuffer-pointer bewaren en na ExitBootServices direct aanspreken.
- [ ] **PCI/PCIe enumeratie** (poort 0xCF8/0xCFC) — basis voor alle drivers.
- [ ] Driver-model: traits voor block-device, input-device — drivers draaien
      later in **userspace** (microkernel-eis), nu nog in-kernel als bootstrap.
- [ ] **NVMe / SATA (AHCI)** storage driver → bestanden van schijf lezen (Q4 2026).
- [ ] **USB HID** toetsenbord + muis → input op fysieke hardware (Q3 2026).
- [ ] GOP → eigen DRM/display mode-setting pad.

**Mijlpalen:** USB-input werkt op hardware (Q3); schijf-lezen (Q4); boot op
referentiehardware Dell/Lenovo ThinkPad (Q4 2026).

### Fase 3 — Kernel internals = Track 3 (Maand 6–30, kritiek pad)
**Doel:** meerdere processen, beschermd geheugen, syscalls, IPC.

Sub-fasering uit Track 3:
- [ ] **3.1 Geheugen** (M6–10): `PhysAddr`/`VirtAddr` types · `BitmapFrameAllocator`
      (init uit UEFI memory map) · slab kernel-heap · 4-niveau paging ·
      recursive mapping · TLB-flush na elke wijziging.
- [ ] **3.2 Scheduler** (M10–14): `Process`/`CpuContext` · context-switch via
      `global_asm!` · APIC-timer (8259 PIC uitzetten!) · CFS-geïnspireerde
      `virtual_runtime` red-black tree.
- [ ] **3.3 GDT/IDT** (M12–16): GDT+TSS · IDT · exception-handlers (page fault,
      double fault via IST-stacks) · ring 0/3 scheiding (SMEP/SMAP).
- [ ] **3.4 Syscalls** (M16–22): `SYSCALL`/`SYSRET` via LSTAR MSR · `swapgs`
      stack-switch · dispatcher · eerste userspace-proces in ring 3 · `exit/read/write`.
- [ ] **3.5 IPC & lifecycle** (M22–30): message-passing IPC, shared memory,
      signals, fork/exec, scheduler-tuning. **Hier verankert capability-based
      security** — capabilities als kernel-objecten die via IPC doorgegeven worden.

**Mijlpalen:** 2 processen + context-switch (M14) · userspace ring 3 (M18) ·
volledige syscall-tabel + shell (M24) · IPC stabiel, klaar voor T4/T5 (M30).

> Let op: **geen `async/await` in kernel-code** — gebruik synchrone polling /
> callbacks. Async is voor userspace.

### Fase 4 — Parallelle bovenbouw (Maand 12–48)
Na Track 3 splitsen twee teams:

**4a. EuroNet = Track 4** (M18–42)
- [ ] 4.1 PCI · **VirtIO-net** (QEMU eerst) · Ethernet · ARP · ICMP-ping (M22).
- [ ] 4.2 IPv4 + checksum · UDP · DHCP-client (M26).
- [ ] 4.3 **TCP-state machine** (11 toestanden) · socket-syscalls 60–72 (M32).
- [ ] 4.4 DNS-resolver · TLS via `rustls`+`ring` · `webpki-roots` (M36).
- [ ] 4.5 IPv6 · Intel e1000 (echte HW) · WiFi-basis (M42).
- *Valkuilen:* big-endian conversies overal (`from_be_bytes`); checksums verplicht;
  VirtIO **modern** spec (1.x), niet legacy; happy-path TCP eerst, edge-cases later.

**4b. EuroDesktop = Track 5** (M20–48)
- [ ] 5.1 Software-renderer naar GOP · `Renderer`-trait · Button/Label/TextInput ·
      vensterdecoraties · demo-scene (M26).
- [ ] 5.2 Font-rendering met **`fontdue`** · glyph-atlas · Inter + Fira Code
      (`include_bytes!`) · tekstvelden (M30).
- [ ] 5.3 **Compositor-protocol** (eigen, IPC-based; Wayland-geïnspireerd, geen
      Wayland-code) · surfaces · double-buffering · z-ordering · damage-tracking ·
      focus/Alt-Tab (M36).
- [ ] 5.4 **Vulkan-backend** · swapchain · glyph-atlas als GPU-texture · animaties ·
      software-fallback behouden (M42).
- [ ] 5.5 Accessibility (WCAG 2.1 AA) · HiDPI · touch · theming · AZERTY/QWERTZ (M48).
- *Valkuilen:* compositor = security-grens (apps tekenen nooit direct op scherm);
  immediate-mode UI eerst, retained-mode + damage-tracking later; alleen opaque
  kleuren tot Vulkan klaar is (alpha-blending is traag in software).

### Fase 5 — Ecosysteem & productrijp (Jaar 2–5)
Uit het memorandum, bovenop de tracks:
- [ ] Eigen minimale **libc** (POSIX-compatibel, Rust) — M3–9, voor toolinghergebruik.
- [ ] **Package manager** — sandboxed app-model, cryptografisch gesigneerde packages.
- [ ] **Self-hosting**: `rustc`-toolchain porten → OS compileert zichzelf (Jaar 2).
- [ ] Eigen **filesystem** (of ext4-lezer) · terminal-emulator · shell.
- [ ] **ARM64** kernel-port → Raspberry Pi (Jaar 2).
- [ ] Browser (**Ladybird**-port) · office-suite · email · bestandsbeheer (Jaar 3–4).
- [ ] UEFI **Secure Boot** (eigen certificaten) — enterprise-eis (Jaar 3).
- [ ] Installatie-wizard · hardware-certificering · LTS + enterprise-support (Jaar 4).
- [ ] EuroKernel **Foundation** + onafhankelijk governance (Jaar 5).

---

## 4. Concrete eerstvolgende acties (deze week)

1. **Repo + toolchain** opzetten (Fase 0): `rust-toolchain.toml`, `.cargo/config.toml`,
   `Cargo.toml`. Verifieer dat `cargo build --release` een `.efi` produceert.
2. **Track 1 implementeren** (Fase 1) — dit is de snelste weg naar een zichtbaar,
   demonstreerbaar resultaat en de basis voor de CI-regressietest.
3. **CI-pipeline** die het `.img` bouwt en in QEMU+OVMF headless boot + screenshot.
4. **Track 2-document inwachten** → Fase 2 verfijnen.

> Wil je dat ik nu meteen de skeletstructuur van **Fase 0 + Track 1** in code
> aanmaak (alle bestanden uit Track 1, build- en run-scripts), zodat `./build.sh`
> direct een bootable image oplevert? Dan kun je het vandaag in QEMU zien draaien.

---

## 5. Risico's op de bouw (technisch, niet zakelijk)

| Risico | Mitigatie |
|---|---|
| ExitBootServices-volgorde fout → stille crash | Memory map vóór exit ophalen; in code-review afdwingen |
| BGR/RGB verwisseld op echte HW | `pixel_format()`-detectie vanaf Fase 1 |
| Context-switch / naked asm instabiel | `global_asm!` i.p.v. `#[naked]`; isoleer in één module |
| `fontdue`/`rustls` trekken stiekem `std` binnen | `cargo tree --no-default-features` controleren in CI |
| TCP edge-cases (retransmit, TIME_WAIT, out-of-order) | Happy-path eerst, edge-cases als aparte gefaseerde taken |
| Microkernel-IPC te traag | IPC vroeg benchmarken (Fase 3.5); het is het hart van perf |
| Capability-model te laat ingebouwd | Vanaf Fase 3.5 ontwerpen, niet achteraf erop plakken |

---

*Laatste update: 2026-06-01. Te herzien zodra Track 2 (HAL/storage/drivers) is toegevoegd.*
