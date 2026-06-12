# EuroOS — Uitgebreide Roadmap & Visie

> Van een werkende from-scratch kernel naar een volwaardig, soeverein Europees
> besturingssysteem. Deze roadmap consolideert het Investor Memorandum, de
> technische tracks (1–6), en de design-/security-/UX-specificaties tot één
> samenhangend, gefaseerd plan.
>
> Leidende keuze: **security-fundament + UI-infrastructuur eerst, dan pas apps op
> schaal.** (De design- en UX-specs benadrukken expliciet dat rijpe UI-infra
> belangrijker is dan méér apps; de security-spec stelt boot-chain + FDE + signing
> als harde voorwaarde.)

---

## 0. Visie

Europa heeft geen eigen desktop-OS. EuroOS is het eerste volledig soevereine,
from-scratch Europese besturingssysteem voor de gewone gebruiker: een microkernel
in Rust, een eigen desktop, **nul telemetrie**, **security-by-design**, EUPL-1.2,
en een ecosysteem dat de Europese community draagt. Doel: bootable prototype maand
12, eindgebruikers-v1.0 maand 48.

**Vier onveranderlijke pijlers** (uit de specs):
1. **Soevereiniteit** — eigen code, geen Amerikaanse/GPL-besmette kernbasis, eigen
   infrastructuur, nul telemetrie.
2. **Security-by-design** — capability-based, sandboxed, gesigneerd, versleuteld —
   vanaf v1, geen afterthought.
3. **Eigen visuele identiteit** — niet Windows/macOS/GNOME; rust, vertrouwen,
   Europese stijl; resolutie-onafhankelijk (1080p → 8K).
4. **Open ecosysteem** — open source, RFC-proces, reproduceerbare gesigneerde
   packages, POSIX-compatibel om bestaande software te ontsluiten.

---

## 1. Waar we nu staan (geleverd & geverifieerd)

Zie `STATUS.md` voor details. Samengevat draait vandaag al, from scratch:

| Pijler | Geleverd |
|---|---|
| Boot + kernelmodus | UEFI → ExitBootServices → eigen GDT/IDT/paging/heap/COM1 |
| Geheugen | EuroMM frame-allocator (UEFI-memmap) |
| Filesysteem | **EuroFS** CoW-FS (inodes, extents, checkpoints, XXH3, crash-consistent) |
| Netwerk | **EuroNet** Ethernet/ARP/IPv4/ICMP/UDP (parse+build+checksums) |
| Multitasking | preemptieve scheduler (timer-IRQ context-switch), kernel- én ring-3-taken |
| Userspace | ring 3 + SYSCALL/SYSRET + ELF64-loader |
| Drivers | IRQ-toetsenbord + PS/2-muis |
| Desktop | **EuroDesktop** compositor: vensters, z-order, sidebar, muis, slepen |
| Toolchain | **EuroToolchain**: C→ELF→ring 3; **eupkg** (Ed25519-gesigneerde packages) |

~5.200 regels eigen code, 118 KB kernel, 55 host-tests. Dit is het **fundament**;
de rest van deze roadmap bouwt erop.

---

## 2. Architectuurstack (doel)

De UX-spec definieert de juiste lagen — apps bouwen **nooit** direct op rendering-API's:

```
Kernel  →  Compositor  →  EuroUI-framework  →  Window Manager  →  Design System  →  Applicaties
                                   ↑
                       EuroIPC (message/event/RPC-bus)  +  Brokers (file, camera, mic, netwerk, klembord)
                                   ↑
   Security-services: Identity · Crypto · EuroVault · Update · Package · Sandbox · Audit
```

---

## 3. De roadmap in horizonten

Elke horizon is een coherent, demonstreerbaar geheel. Tracks lopen parallel waar mogelijk.

### Horizon A — Fundament afmaken *(de basis betrouwbaar maken)*
**Doel:** een veilige, multitaskende kernel met de UI-infrastructuur waarop alles rust.

- **Kernel-internals afronden**
  - Syscall-tabel uitbreiden: `open/read/close/seek/stat/readdir/mkdir/unlink`,
    `mmap/munmap`, `fork/exec/wait`, `pipe`, `nanosleep` (POSIX-semantiek, eigen nummers).
  - Meerdere ring-3 processen door de scheduler (per-taak kernel-stacks + TSS.rsp0-update).
  - **EuroMM eigen slab-allocator** (vervangt `linked_list_allocator`); kernel-heap groei.
  - APIC i.p.v. PIT; daarna **SMP** (meerdere cores).
  - **EuroIPC**: message-/event-bus + RPC met **app-identiteit, permissie-checks,
    rate-limiting, audit-hooks** (security-spec).
- **Security-fundament** *(security-spec, Fase 1 — harde voorwaarde)*
  - **Secure-boot-keten**: Firmware/UEFI → EuroBoot → kernel → services → sessie → apps;
    elke stap verifieert de volgende. Gesigneerde bootloader/kernel/drivers, rollback-protectie.
  - **Measured boot** (TPM 2.0 / PCR) waar hardware het ondersteunt; remote-attestation-haakjes.
  - **Capability-tokens** als kernel-primitief (vervangt root/non-root): proces krijgt
    exact de rechten die het nodig heeft, **revocable**, gebonden aan app-identiteit.
  - **Gesigneerde executables**: ongesigneerde binaries draaien niet (developer-mode apart).
- **EuroFS security-uitbreiding**
  - **Native encryptie** per-file (XChaCha20-Poly1305 of AES-256-GCM), per-file keys,
    extended attributes, **quarantine-flag**, **immutable-flag**, snapshots, ACL, integrity-metadata.
- **UI-infrastructuur** *(UX-spec — "belangrijkste volgende stap, niet méér apps")*
  - **EuroUI-framework**: Layout Engine · Rendering Engine · Theme Engine · Animation
    Engine · Accessibility Engine. Apps bouwen hierop.
  - **DPI-engine + resolutie-onafhankelijkheid**: de abstracte eenheid **`eu` (Euro Unit)**;
    schaalstappen **1080p→100%, 1440p→125%, 4K→150%, 5K→200%**.
  - **Design System (EDS) tokens** vastleggen:
    - Grid: basiseenheid **4**; spacing **4/8/12/16/24/32/48/64/96**; padding **4/8/12/16/24/32/48**.
    - Radius: **Small 8 · Medium 12 · Large 20 · XL 28**; elevation-niveaus 0–4; borders Thin/Regular/Strong/Accent.
    - Typografie: **Inter** (UI), **JetBrains Mono** (terminal; alt Fira Code / IBM Plex Mono);
      type-schaal Display/Heading/Title/Body/Caption/Mono.
    - **Security-kleurtaal** als eersteklas semantiek: **Groen = Geverifieerd, Blauw =
      Beschermd, Geel = Aandacht, Rood = Gecompromitteerd, Grijs = Onbekend.**
    - **SVG-only iconenset** (24×24 basis; 16/20/24/32/48/64), géén emoji/bitmaps.
    - Motion: Fast 100ms / Normal 200ms / Slow 300ms; **nooit > 500ms**; 60fps min.
  - Compositor verdiepen: layer-model (wallpaper/desktop/window/overlay/notification/cursor),
    blur/transparantie, schaduw, **animaties**, **venster-resize**, multi-monitor-haakjes.
  - **Window-chrome security-controls** (novel): naast sluiten/minimaliseren ook
    **Security State** en **Permission State**; persistente titelbalk-indicatoren
    (versleuteld/sandboxed/netwerk/microfoon/camera/locatie/klembord/USB).
  - **Command Palette** (CTRL+SPACE): apps/instellingen/bestanden/commando's.
  - **Workspace-systeem** (Work/Development/Security/Research/Personal); workspace-aware notificaties.
- **Performance-doelen vastpinnen** (UX-spec): UI-respons < 50 ms, venster-open < 150 ms,
  animaties ≥ 60 fps, input-latency < 20 ms, desktop-idle < 300 MB RAM.

**Mijlpaal A:** veilige boot-keten + versleutelde EuroFS + EuroUI/DPI/design-tokens +
compositor met security-indicatoren; een ring-3 shell-app op EuroUI.

### Horizon B — Toolchain volwassen + eerste echte apps
**Doel:** bestaande open-source software bouwen/draaien, en de eerste EDS-apps.

- **Toolchain (Track 6) volwassen** *(verder op wat al draait)*
  - **musl-libc** integreren → echte `printf`/`malloc`/`pthread`; target-triple
    `x86_64-eurokernel`. Dynamic linking (`ld-eurokernel.so`) na statisch.
  - **POSIX-laag** compleet → **bash, curl, git, SQLite, Python 3** compileren ongewijzigd.
  - **Sysroot + cross-compiler + Docker-build** voor reproduceerbare builds (SBOM, SOURCE_DATE_EPOCH).
  - **Linux-ABI-compat-laag** (later): gecompileerde Linux-ELF's draaien via syscall-vertaling.
- **App-security-model** *(security-spec, Fase 2)*
  - **App-manifest** (YAML) met gedeclareerde permissies (filesystem/network/devices/clipboard/ipc).
  - **Sandboxed runtime**: eigen namespace, beperkte FS-/IPC-/netwerk-view, geen directe device-toegang.
  - **Brokers**: secure **file-picker** (capability-token, geen ambient home-toegang),
    camera-/microfoon-/locatie-/USB-/printer-broker — elke toegang zichtbaar.
  - **Parser-isolatie verplicht** voor PDF/Office/afbeeldingen/codecs/archieven/fonts/browser.
- **Eerste core-apps (EDS-conform)** *(apps-spec, Fase 1)*
  - **EuroFiles** (bestandsbeheer: EuroFS/FAT32/exFAT/NTFS/EXT4/XFS/BTRFS; ACL, signature-view).
  - **EuroTerminal** (shell, pipes, scripting, SSH-client; GPU-versneld, tabs/split-panes).
  - **EuroSettings** (dashboard-model: Accounts/Privacy/Security/Network/Hardware/Accessibility/Updates/Storage/Developer).
  - **EuroMonitor** (taakbeheer: processen/CPU/RAM/netwerk/disks).
- **EuroStore v0.1 + eupkg install** *(verder op de huidige eupkg)*
  - `.eupkg` (ZIP + MANIFEST.toml + Ed25519 + SHA256, **reproduceerbaar, sandboxed**);
    `eupkg install` haalt uit een Europese repo; **Security Score** + permissie-review bij installatie.

**Mijlpaal B:** bash/curl/git draaien op EuroOS; EuroFiles/Terminal/Settings/Monitor in EDS-stijl; EuroStore installeert gesigneerde packages.

### Horizon C — Productiviteit + data-bescherming
**Doel:** een dagelijks bruikbare desktop voor early adopters.

- **Communicatie & PIM** *(apps-spec, Fase 2)*
  - **EuroMail** (IMAP/POP3/SMTP; **TLS verplicht, S/MIME, OpenPGP**; tracking-pixels geblokkeerd;
    Canvos/Mailcow-autodiscovery; native EuroUI, geen webview).
  - **EuroCalendar** (ICS/CalDAV), **EuroContacts** (CardDAV).
  - **EuroPDF** (lezen/annoteren/**digitale handtekeningen**; JavaScript uit, sandboxed).
- **Data-bescherming** *(security-spec, Fase 3)*
  - **Full-disk-encryptie standaard aan** (Argon2id-KDF, HKDF; **recovery-key verplicht +
    offline exporteerbaar**; TPM-ondersteuning + passphrase-fallback).
  - **Per-user encryptie-domeinen**; sleutelhiërarchie Device→System→User→{Documents,Downloads,…}.
  - **Beschermde mappen** (geen auto-app-toegang tot Documents/Desktop/Downloads/Pictures/Mail/Vault/SSH).
  - **EuroVault**-service (wachtwoorden/SSH-keys/certificaten/secrets; nooit plaintext op disk;
    memory-zeroization; hardware-backed via TPM/FIDO2/smartcard).
  - **Privacy-dashboard** + realtime-indicatoren (camera/microfoon/scherm/locatie altijd zichtbaar).
- **Office & media** *(apps-spec, Fase 3)*
  - **EuroWriter** (ODT/DOCX/PDF/MD; macro's uit), **EuroSheets** (XLSX/CSV; formule-validatie),
    **EuroSlides** (PPTX); **EuroPhotos**, **EuroMedia** (sandboxed codecs).
- **Identiteit & login**
  - **Login-manager**: multi-user, **Passkeys/FIDO2/smartcard/eID (eIDAS-ready)**; toont
    encryptie-/secure-boot-/integriteitsstatus. Tijdelijke privilege-escalatie met audit.

**Mijlpaal C:** beta voor early adopters — versleutelde desktop, mail/agenda/contacten,
office, EuroVault; eerste overheidspilots.

### Horizon D — Threat-resistance, ecosysteem & enterprise
**Doel:** productierijp, schaalbaar ecosysteem, enterprise-klaar.

- **Threat-resistance** *(security-spec, Fase 4)*
  - **Anti-ransomware**: gedragsdetectie (mass-changes, snelle encryptie-patronen), **shadow-versies**,
    auto-pauze verdachte apps, rollback.
  - **Immutable snapshots**, **USB-bescherming** (untrusted-mount, HID-injection-detectie, allowlist),
    quarantine-flags, **app-revocation** (cert/hash-blocklist).
- **Browser & web** *(apps-spec)*
  - **EuroWeb** (Firefox-basis, telemetrie gestript, eigen EuroDesktop-UI; site-/process-isolatie,
    TLS 1.3, HSTS, Certificate Transparency, geen third-party cookies). Ladybird als langetermijndoel.
- **Beheer & toegankelijkheid**
  - **EuroFirewall**, **EuroCertificates** (trust-stores, smartcards, eID), **EuroDisk** (SMART),
    **EuroBackup** (client-side encryptie, ransomware-resistente snapshots), **EuroRemote** (SSH/RDP/VNC, MFA).
  - Volledige **toegankelijkheid** (schermlezers, toetsenbord-only, hoog contrast, colorblind-modi,
    reduced motion), **multi-monitor + HDR**, snap-layouts.
- **Enterprise** *(security-spec, Fase 5)*
  - Policy-management, **remote attestation**, **SIEM-export** (Syslog/JSONL/OpenTelemetry),
    centrale update-controle, compliance-rapportage, **Trust Center** + device-compliance.
- **Schaal & platform**
  - **ARM64-port** (Raspberry Pi), self-hosting (`rustc` op EuroOS), GPU/**Vulkan**-compositor-backend
    (Vello/WGPU als richting), **EuroKernel Foundation** + onafhankelijk governance.

**Mijlpaal D:** v1.0 voor eindgebruikers; certified-hardware-programma; enterprise-support; institutionele adoptie.

---

## 3a. Track 7 — EuroGuard (systeembrede toegangs- & netwerkcontrole)

*Een dwarsdoorsnijdende security-track (spec: Track7_EuroGuard v0.1). Geen enkel desktop-OS
biedt gewone gebruikers dit niveau van transparantie en controle — Android deed het voor mobiel,
EuroOS doet het voor desktop. EuroGuard bouwt op ons bestaande capability-fundament (kernel-checks
op syscalls, verify-before-execute) en de net opgeleverde socket-laag (`connect/send/recv`).*

**Drieniveau-policy:** Systeem > Gebruiker > App (een systeemregel kan een app nooit overschrijven).
Permissiecategorieën: `fs.*`, `net.*`, `hw.*`, `sys.*`, `priv.*`; tijdsmodi ALTIJD/ALLEEN_BIJ_GEBRUIK/
EENMALIG/WEIGEREN/VRAGEN ("alleen bij gebruik" als standaard-aanbeveling).

| Fase | Inhoud | Horizon | Status |
|---|---|---|---|
| 7.1 | **Permissie-framework in kernel** — policy-checks op `SYS_CONNECT/OPEN/BIND`; policy-cache per proces | A | **eerste snede geleverd** (connect-enforcement + per-app-id) |
| 7.2 | App-policy-opslag + policy-engine — TOML, drieniveau-hiërarchie, cache-invalidatie | A/B | gepland |
| 7.3 | Permissiedialoog-UI + statusbalk-indicator (EuroGuard-daemon ↔ UI, IPC) | B | gepland (na per-process + EuroIPC) |
| 7.4 | **Netwerkstatistieken per app** — per-verbinding + per-app aggregatie, DNS-query-log, GeoIP (MaxMind GeoLite2, lokaal) | B | **eerste snede geleverd** (per-app bytes/connects/blocks) |
| 7.5 | **DNS-over-HTTPS** + blokkeerlijsten (ads/trackers/malware/coinminers), Europese DNS (Quad9) | B/C | gepland (na TLS) |
| 7.6 | EuroGuard-daemon (userspace) — brug kernel ↔ UI | B/C | gepland |
| 7.7 | EuroGuard-UI — permissie- + netwerk-dashboard (Netwerkmonitor, per-app-detail, verbindingskaart) | C | gepland |
| 7.8 | **Audit logging** — versleuteld lokaal, GRANT/REVOKE/BLOCK/CONNECT/ALERT, CSV/JSON-export, rotatie | C | **eerste snede geleverd** (in-kernel audit-ring + `guard`-commando) |
| 7.9 | Anomalie-detectie + waarschuwingen (ongewoon datavolume, nieuw IP, achtergrond-misbruik) | C/D | gepland |
| 7.10 | Privacy-modus, sandbox-profielen (onvertrouwd/privacy/ontwikkelaar) | D | gepland |
| 7.11 | VPN-integratie (kill-switch, split-tunneling), verbindingskaart | D | gepland |
| 7.12 | Gedragsanalyse, data-budgetten per app, netwerk-tijdsschema's, familieprofiel/kinderveiligheid | D | gepland |

**Mijlpalen (uit de spec):** A — beveiligingsfundament (harde policy-grens, geen cosmetics) ·
B — zichtbaarheid (dashboard) · C — controle (regels/audit) · D — intelligentie (anomalie/gedrag).
**Valkuilen om te bewaken:** dialoogmoeheid (toon op het relevante moment, slimme defaults),
performance (policy-beslissing cachen — één lookup per verbinding, niet per pakket), GeoIP lokaal
(geen cloud-lookup van IP's), gesigneerde blokkeerlijst-updates van eigen Europese server.

> **Nu al draaiend (deze run):** een echte EuroGuard-kern in de kernel — een policy-engine met een
> systeem-blocklist, per-app netwerkstatistieken en een audit-ring — ingehaakt op de `connect`-syscall.
> Een geblokkeerde "tracker"-app wordt door de kernel tegengehouden en gelogd; het `guard`-shellcommando
> toont de Netwerkmonitor + Auditlog. Dit is Fase A (de harde policy-grens), verifieerbaar in QEMU.

---

## 4. Dwarsdoorsnede — vaste ontwerpregels (uit de specs)

**Standaard-instellingen (security-spec, "secure by default"):** FDE aan · per-user-encryptie
aan · firewall aan · app-sandboxing aan · ongesigneerde apps geblokkeerd · developer-mode uit ·
telemetrie uit · USB-autorun uit · downloads niet uitvoerbaar · third-party-cookies uit ·
document-macro's uit · PDF-JavaScript uit · backup-encryptie aan · scherm-opname-indicator altijd zichtbaar.

**Release-1 security-acceptatiecriteria (de "gate"):** gestolen disk = geen user-data ·
gewone app kan Documents niet lezen · browser-exploit ontsnapt niet zonder tweede exploit ·
ongesigneerde binary draait niet · update zonder geldige handtekening installeert niet ·
malware kan niet massaal versleutelen zonder detectie · geen plaintext-secrets · logs zonder
secrets · USB triggert geen auto-execute · recovery omzeilt geen encryptie.

**Security-UX-regel:** verberg **nooit** permissie-verzoeken, netwerk-toegang, encryptie-fouten
of certificaat-waarschuwingen. Elk scherm beantwoordt: *Wat is dit? Wat kan ik doen? Is het veilig?
Wat gebeurt er nu?*

---

## 5. Technische beslissingen — master-lijst

| Domein | Keuze |
|---|---|
| Kerntaal | **Rust** (kernel + kritieke userspace); C/C++ alleen gesandboxed + seccomp-achtig + parser-fuzzing |
| Rendering (doel) | Vello / WGPU (Skia als alternatief); **geen** pixel-UI of bitmap-iconen |
| Crypto | XChaCha20-Poly1305, AES-256-GCM, ChaCha20, **Argon2id**, HKDF |
| Hardware-security | **TPM 2.0** (measured boot/PCR), secure-enclave-achtig, **FIDO2**, smartcard, HSM (enterprise) |
| Auth/identiteit | **Passkeys, FIDO2, eID/eIDAS-ready**, S/MIME, OpenPGP |
| Web/netwerk | HTTP/2, HTTP/3, **TLS 1.3**, HSTS, Certificate Transparency, DoH, cert-pinning |
| Mail/PIM | IMAP/POP3/SMTP, ICS/CalDAV, CardDAV |
| Remote | SSH/SCP/SFTP, RDP, VNC, MFA |
| Filesystemen | **EuroFS** (native enc, per-file keys, snapshots) + FAT32/exFAT/NTFS/EXT4/XFS/BTRFS lezen |
| Supply-chain | gesigneerde binaries/packages, **reproduceerbare builds, SBOM**, hash+dep-verificatie, atomic/layered updates + rollback |
| Observability | Syslog, JSONL, OpenTelemetry, SIEM-API (enterprise, opt-in) |
| Fonts | Inter, Noto Sans, JetBrains Mono, Fira Code, IBM Plex Mono |
| UI-eenheid | **`eu` (Euro Unit)**, grid-basis 4 |
| Licentie | **EUPL-1.2** |

---

## 6. Te bouwen OS-componenten (naamlijst, uit de specs)

EuroUI-framework · Compositor · DPI-engine · Theme-engine · Animation-engine ·
Accessibility-engine · Window Manager · Workspace Manager · SVG-iconensysteem ·
EuroIPC · EuroFS · EuroBoot · **EuroVault-service** · Crypto-service · Identity-service ·
Update-service · Package-service · Notification-service · Search-Index-service ·
Print-service · Network-service · Service Manager · brokers (file/camera/microfoon/
locatie/USB/printer/Bluetooth/klembord/netwerk).

**21 core-applicaties** (apps-spec): EuroFiles · EuroBrowser/EuroWeb · EuroTerminal ·
EuroSettings · EuroMonitor · EuroWriter · EuroSheets · EuroSlides · EuroPDF · EuroMail ·
EuroCalendar · EuroContacts · EuroPhotos · EuroMedia · EuroVault · EuroFirewall ·
EuroCertificates · EuroDisk · EuroBackup · EuroRemote · EuroStore.

---

## 7. Sequencing-inzicht (belangrijk)

De drie spec-families wijzen op één volgorde, ondanks per-document-fasenummers:

1. **Eerst** het **security-fundament** (boot-chain, FDE, signing, capabilities) — harde voorwaarde.
2. **Parallel** de **UI-infrastructuur** (EuroUI, DPI, design-system, compositor, SVG-iconen) —
   "de belangrijkste volgende stap is niet méér apps".
3. **Dan pas** de **apps op schaal** (in de apps-spec-volgorde: Files/Terminal/Settings/Monitor →
   Mail/Calendar/Contacts/PDF → Writer/Sheets/Slides → Vault/Firewall/Certificates → Backup/Remote/Store).
4. **Doorlopend** de **toolchain/POSIX/Store** (ontsluit bestaande software) en **threat-resistance/enterprise**.

Ons huidige werk (Tracks 1–6) heeft het kernel- en toolchain-fundament al gelegd én een
eerste compositor + desktop. Horizon A maakt het security- en UI-fundament af; daarna stapelen de apps.

---

*Bronnen: Investor Memorandum 2026 · Tracks 1–6 · EuroOS Core Applications Spec · EuroOS EDS
Master Design System · EuroOS Security/Encryption/Data Protection · EuroOS UI/UX Architecture.*
*Laatste update: 2026-06-01.*
