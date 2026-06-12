# EuroOS — Fase-2 Sprintplan (post I/J)
*Opgesteld 2026-06-05 · synthese van de resterende roadmap (K–Z + EuroSuite) + eigen voorstellen, gefilterd op waarde × haalbaarheid × ontgrendeling.*

---

## 0. Waar we staan (eerlijke nulmeting)

EuroOS boot vandaag tot een desktop met: persistente EuroFS (A/B-updates, scrub, concurrente cache), **echte USB** (xHCI HID + mass-storage), **HDA-audio**, MSI-X, transparante swap, SMP + per-CPU-runqueues, ring-3 + dynamische linker (H3) + WASM (H4) + Wayland (H5), EuroGuard-capabilities + containers, TCP/TLS 1.3/DHCP/DNS, ACPI + **AML-interpreter** (I3), lock-vrije kmsg. **255 host-tests, 15 lib-crates.**

De kernel is **technisch volwassen maar nog geen dagelijkse-gebruiker-OS**. De échte gaten zijn niet meer "ontbrekende kernelfeatures" maar: (a) de drivers zijn ad-hoc gebouwd (techschuld), (b) er is geen GPU/efficiënte display voor echte schermen, (c) geen WiFi, (d) de **soevereiniteits-ruggengraat** (immutability, TPM, FDE, policy, secrets, audit, attestatie) — de eigenlijke USP — is nog grotendeels gepland i.p.v. gebouwd, en (e) geen installer om het op echte hardware te zetten.

## 1. Prioriteringsprincipes

1. **Schuld vóór breedte.** We hebben net 2 drivers (xHCI/HDA) ad-hoc gebouwd. Vóór WiFi/GPU/USB-hubs komt het **driver-framework (R)** — anders vermenigvuldigt de schuld.
2. **De USP eerst, niet feature-pariteit.** EuroOS wint niet door Linux na te bouwen, maar door **soevereiniteit + veiligheid** (EuroGuard, immutability, TPM-sealed, capability-policy, audit). Dat is de differentiator → die ruggengraat heeft voorrang op app-breedte.
3. **Verifieerbaar in deze omgeving.** TCG-QEMU, geen KVM, geen echte hardware. Host-tests + boot-verificatie zijn koning. Items die alléén op echte hardware te bewijzen zijn (S3-suspend, GPU op echte kaart, WiFi-radio) worden eerlijk gemarkeerd en zo gebouwd dat de *logica* host-getest is en de *integratie* gelabeld attended.
4. **Betrouwbaarheid is een feature.** Snapshots, crash-dumps, health, observability maken het OS *vertrouwbaar in productie* — relatief goedkoop op de bestaande CoW/scrubber/kmsg-fundamenten.
5. **Afmaken wat begonnen is.** J2 (MSI-X→completion-pad) en de I3-randen (S5-shutdown, S3) eerst afronden vóór nieuwe sporen.

## 2. Zinvolle toevoegingen — de filter (incl. eigen voorstellen)

Naast de reeds gedocumenteerde K–Z + EuroSuite stel ik deze **eigen toevoegingen** voor, die echte gaten dichten en goedkoop+verifieerbaar zijn (gemarkeerd ★NEW):

| Item | Waarom zinvol | Haalbaarheid |
|------|---------------|--------------|
| ★NEW **E2E-interactiviteitstest** | xHCI-kbd werkt nu; een geautomatiseerde "QMP typt commando → shell voert uit → output geverifieerd" bewijst de hele invoer→shell→FS-lus. Vangt regressies in de complete stack. | Hoog — QMP `send-key` + serial-assert. ½ sessie. |
| ★NEW **ACPI `_S5`-gedreven shutdown** | I3 evalueert nu `\_S5` uit de echte DSDT; gebruik die SLP_TYP-waarden voor de échte shutdown (i.p.v. hardcoded PM1a). Sluit de AML-lus + nette poweroff/reboot. | Hoog — kernel heeft PM1a_CNT al; AML levert SLP_TYP. ½ sessie. |
| ★NEW **CPU-idle (HLT/C-states) + tickless-idle** | De idle-loop spint nu; een echte `hlt`-idle + (later) ACPI C-states bespaart energie — relevant voor "daily driver" + laptops. | Middel — idle-task `hlt`; C-states via AML `_CST`. 1 sessie. |
| ★NEW **EuroFS `fsck`/repair** | Scrub *detecteert*; een repair-tool dat A/B-superblok + bitmap + object-map herstelt maakt het FS écht crash-bestendig (★ de top-reliability-risk uit de analyse). | Hoog — host-testbaar met gecorrumpeerde images. 1–2 sessies. |
| ★NEW **Syscall-audit-trace (capability-gated)** | Bindt P3 (audit) + X (policy) + W (observe): elke capability-check + -denial gelogd → de *security-observability*-story (NIS2/GDPR-bewijslast). | Hoog — haakt in bestaand syscall-pad + lock-vrije ring. 1 sessie. |
| **Finish J2** (begonnen) | MSI-X-levering is bewezen; nu het virtio-blk/NVMe-**completion-pad** van busy-poll → IRQ. Echte perf-/efficiëntiewinst. | Middel — legacy-virtio-header schuift +4 bij MSI-X; zorgvuldig. 1 sessie. |

**Afgewezen/uitgesteld als niet-zinvol-nu:** A3 hypervisor, A5 distributed storage, A13 AI-runtime (te vroeg); real-time CRDT-collaboratie in EuroSuite (na MVP); `.doc`/`.xls` legacy (conversiehint volstaat).

## 3. Het fasenplan

Vijf samenhangende fasen, elk met een concreet milestone. Binnen een fase: host-test → boot-verify → docs, zoals altijd.

### Fase 2A — Consolideren & afmaken *(fundament-schuld aflossen)*
> **Milestone:** "Eén device-model, geen losse drivers; begonnen werk af."

| Sprint | Inhoud | Deps | Verify | Inschatting |
|--------|--------|------|--------|-------------|
| **R EuroDevice** | Driver-framework: `DeviceTree`/`DriverRegistry`/`trait Driver`/hotplug-bus. **Migreer PCI/NVMe/VirtIO/xHCI/HDA** als referentie. | bestaande drivers | `eurodevice probe` toont boom; bestaande boot-tests blijven groen; mock-hotplug → callback | 2–3 |
| **Finish J2** | virtio-blk/NVMe completion van poll → MSI-X-IRQ (header-shift correct). | J2-fundament ✅ | IRQ-gedreven I/O; teller > 0; alle FS-tests groen | 1 |
| ★ **`_S5`-shutdown + HLT-idle** | AML-`_S5` → echte ACPI-poweroff; idle-task `hlt`. | I3 ✅ | `[pm]` nette poweroff via DSDT-SLP_TYP; idle-CPU% omlaag | 1 |
| ★ **E2E-interactiviteitstest** | QMP-keystrokes → shell-commando → FS-effect geverifieerd. | xHCI ✅ | `run-e2e.py`: typ `ls /` + `cat`, assert serial-output | ½ |

### Fase 2B — Soevereine veiligheids-ruggengraat *(de USP)*
> **Milestone:** "Onveranderbaar, hardware-verankerd, beleid-gestuurd — auditeerbaar."

| Sprint | Inhoud | Deps | Verify | Inschatting |
|--------|--------|------|--------|-------------|
| **L1 + L2** | EuroFS immutable-inode-flag + `CAP_IMMUTABLE_ADMIN`. | EuroFS, EuroGuard | append-only/immutable afgedwongen; host-tests | 1–2 |
| **P3 audit-log** | Append-only audit-log (op L1) — event-source voor U/X/W. | L1 | elke gevoelige actie gelogd, niet-overschrijfbaar | 1 |
| ★ **Syscall-audit-trace** | capability-checks + denials → P3 + lock-vrije ring. | P3, EuroGuard | denial-event met binary+cap zichtbaar | 1 |
| **O1 TPM 2.0** | TPM-MMIO-discovery (ACPI) + PCR-extend + seal/unseal. | I3/ACPI | QEMU `tpm-tis` + swtpm: PCR's gelezen, seal-roundtrip | 2 |
| **K3 FDE** | Full-disk-encryptie (XTS-AES) met TPM-sealed key (O1) + PCR-policy. | O1, eurotls-crypto | volume ontsleutelt enkel bij juiste PCR's | 2 |
| **X EuroPol** | Declaratieve TOML-policy → EuroGuard-capability-grants, syscall-enforcement. | EuroGuard, P3 | `europol apply` → `EPERM`; `explain` toont regel | 2 |
| **U EuroVault** | Capability-gated, TPM-sealed secrets-store (AES-256-GCM), `Zeroizing`. | O1, P3 | proces zonder cap → `EPERM`; secret 0x00 na drop | 2 |

### Fase 2C — Betrouwbaarheid & operabiliteit *(productie-vertrouwen, goedkoop)*
> **Milestone:** "Snapshots, crash-dumps, metrics, health — productie-waardig."

| Sprint | Inhoud | Deps | Verify | Inschatting |
|--------|--------|------|--------|-------------|
| **S EuroSnap** | CoW-snapshots + rollback; **G4-integratie** (auto-rollback faalde update). | EuroFS-CoW, G4 | snapshot→schrijf→rollback→data weg, FS intact | 2 |
| ★ **EuroFS fsck/repair** | A/B-superblok + bitmap + objmap-herstel. | EuroFS | gecorrumpeerde image → repair → mount OK | 1–2 |
| **Y EuroCrash** | Kernel crash-dumps (mini/full) + recovery-boot (pre-paging-safe writer). | G1, L1, G4-loader | geforceerde `#DF` → dump → `eurocrash backtrace` | 2–3 |
| **W EuroObserve** | In-kernel lock-vrije metrics + OpenMetrics-endpoint + W3C-tracing. | lock-vrije-kmsg ✅, EuroNet | `curl /metrics` → Prometheus scrapet | 2 |
| **Z EuroHealth** | SMART + FS-health + mem-diag daemon → W + EuroDisplay-alerts. | NVMe, scrubber, W | `eurohealth disk` toont SMART; alert bij drempel | 2 |

### Fase 2D — Echte-hardware dagelijkse gebruiker
> **Milestone:** "Draait op een echte laptop: WiFi, scherm, suspend, installeerbaar."

| Sprint | Inhoud | Deps | Verify | Inschatting |
|--------|--------|------|--------|-------------|
| **N1 WiFi** | 802.11 infra + WPA3-SAE; Intel AX200/AX210 (of USB-WiFi via I1). | I1 ✅ | (attended, echte radio) protocol-kern host-getest | 3–4 |
| **K4 GPU / versnelde display** | virtio-gpu (2D blit/scanout) → later Vulkan-groundwork. | display | QEMU `virtio-gpu`: scanout-resolutiewissel + blit | 2–3 |
| **N2 + N3** | WireGuard + packet-filter. | EuroNet | tunnel up; filterregels afgedwongen | 2–3 |
| **I3-rest: S3-suspend** | ACPI S3 + `_TMP`/`_BST` (AML side-effects/EC). | I3 ✅ | (attended; niet headless) AML-`_PTS`/`_WAK` | 2 |
| **Q1 installer** | Begeleide installer (partitionering, locale, K1-user, K3-FDE-enrol). | K1, K3, GPT | verse schijf → geïnstalleerd bootend OS | 2–3 |

### Fase 2E — Identiteit, containers & applicaties
> **Milestone:** "Enterprise-identiteit, OCI-containers, het eerste eigen kantoor-document."

| Sprint | Inhoud | Deps | Verify | Inschatting |
|--------|--------|------|--------|-------------|
| **V EuroIDM** | Pluggable identity (local/LDAP/OIDC) → capability-mapping + SSO. | K1, TLS, H5 | LDAP-login → desktop-sessie + caps | 3 |
| **T EuroContainer** | OCI-containers op EuroGuard + EuroFS-overlay + Ed25519-registry. | H4, S | `euroctr run` geïsoleerd; cap-deny werkt | 3–4 |
| **EuroSuite ES-Core + ES-IO** | Universal Document Model + OOXML/ODF/PDF-I/O met round-trip-compat. | H5, fonts | `.docx` lezen→model→schrijven == Word-output | groot (multi-maand product) |
| **EuroSuite Writer/Calc/Impress** | De drie apps op Slint. | ES-Core/IO | per-app MVP-milestones (zie EUROSUITE-PLAN.md) | groot |

## 4. Aanbevolen volgorde — de eerste 6 sprints

1. **R EuroDevice** — *nu*, vóór elke nieuwe driver. Lost de grootste architecturale schuld op, volledig host-testbaar, en maakt N1/K4/USB-hubs daarna goedkoop. **Hoogste hefboom.**
2. **Finish J2 + `_S5`-shutdown + HLT-idle + E2E-test** — een korte "afmaak-sprint": rondt begonnen werk af en levert een nette poweroff + energiezuinige idle + een regressie-vangende E2E-test. Veel waarde per regel.
3. **L1 + L2 + P3** — de basis van de soevereiniteits-ruggengraat (immutability + audit). Klein oppervlak, hoge veiligheidswaarde, ontgrendelt U/X/Y.
4. **S EuroSnap + EuroFS-fsck** — betrouwbaarheid op het bestaande CoW-fundament; versterkt G4 (★ de top-reliability-risk) en maakt updates omkeerbaar.
5. **O1 TPM + K3 FDE** — hardware-verankerd vertrouwen; samen de "verified+encrypted"-belofte. (TPM via QEMU `tpm-tis`+swtpm — wél verifieerbaar.)
6. **X EuroPol + W EuroObserve** — beleid-enforcement + metrics; kleine, hoogwaardige toevoegingen die de USP zichtbaar maken.

> Daarna: U → Y → Z (operabiliteit afmaken) → N1/K4/Q1 (echte hardware) → V/T → EuroSuite.

## 5. Wat ik bewust uitstel — en waarom

- **A3 native hypervisor, A5 distributed storage, A13 AI-runtime** — een volledig apart project / te vroeg; herzien na een stabiele v1.
- **Real-time collaboratie (CRDT) in EuroSuite** — pas na de office-MVP.
- **Legacy `.doc`/`.xls`** — conversiehint naar OOXML volstaat; niet de moeite.
- **A11 hardware-compat-DB, A14 sovereign-cloud** — community/infra-taken (→ Q3-governance), geen kernelcode.

## 6. Verifieerbaarheids-noot (TCG-sandbox)

Wél headless-verifieerbaar: R, J2, `_S5`, HLT-idle, E2E, L1/L2/P3, audit-trace, S, fsck, **O1 (swtpm)**, **K3 (FDE-roundtrip)**, X, U, W, Z (QEMU-SMART), K4 (virtio-gpu), N2/N3-logica, EuroSuite (alles host-test). **Attended/echte-hardware:** N1-radio, I3-S3-suspend, GPU op echte kaart, Q1-installer-op-echte-schijf. Voor die laatste bouwen we de *logica* host-getest en labelen we de *integratie* expliciet.

---
*Volgende stap: zeg "do sprint R" (aanbevolen) of kies een fase/sprint. Elk item wordt geleverd met host-tests, een boot-verify, en bijgewerkte docs — zoals Sprints G–J.*
