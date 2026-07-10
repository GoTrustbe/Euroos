# EuroOS — Sprintplanning AD–AG ("Promise = Reality")

*Sprintplanning afgeleid van de backlog in `docs/AGENT-BRIEFING.md` (§7). Doel van deze cyclus: **de kloof sluiten tussen wat we publiek beloven (Zero Trust for AI agents, sovereign identity) en wat de code aantoonbaar levert** — in volgorde van belofte-risico. Elke sprint volgt het vaste patroon: host-geteste crate-core → dunne kernel-glue → `[xx]` boot-zelftest → docs/status bijwerken.*

**Conventies.** Schatting in **sessies** (zoals Sprint K1 = 3–4 sessies). Soort: `N 🔒` = nieuw/security-kritisch, `R` = rewire/refactor, `U` = uitbreiding. Elke taak is pas klaar volgens de *definition of done* uit de briefing: tests groen + boot-geverifieerd + docs + eerlijk statuslabel. De harde regels (§1 van de briefing) gelden onverkort — vooral **nooit fake-as-real** en **`[mock]`-labeling**.

**Volgorde & afhankelijkheden in één blik:**

```
Sprint AD (P0 — EuroAgent echt)        ──┐
  AD-1 tools → AD-2 LLM-default → AD-3 audit-persist
                                          ├─→ Sprint AF (P2 — Zero-Trust-gaten)
Sprint AE (P1 — EuroID end-to-end)     ──┘      AF-1 PCR-seal · AF-2 JIT · AF-3 anomaly
  AE-1 persist → AE-2 login-rewire
                                          └─→ Sprint AG (P3 — breedte, optioneel/parallel)
```

AD en AE kunnen desnoods parallel (raken verschillende modules), maar AD eerst is de aanbeveling: het is de flagship-belofte op /platform/ en /zero-trust/. AF bouwt op beide (AF-2 gebruikt AD-1's tool-pad; AF-1's seal beschermt sleutels die AE-1 persistent maakt). AG is onafhankelijke breedte.

---

## Sprint AD — EuroAgent: van bewezen keten naar echt werkende runtime `N 🔒`

**Sprintdoel.** De agent-runtime levert wat /platform/ en /zero-trust/ adverteren: échte tools (netwerk + secrets) achter capabilities, het échte lokale model als standaardpad, en een audit-trail die een reboot overleeft. Na deze sprint is "Zero Trust for AI agents, enforced at the OS level" geen architectuurclaim meer maar een demo die je kunt draaien.

**Schatting:** 3–4 sessies · **Afhankelijkheden:** geen (alles aanwezig: EuroNet `http_fetch`/`fetch_full`, EuroVault, MCP-gateway, P3 append-only). · **Backlog:** P0.1, P0.2, P0.3.

### AD-1 — Echte MCP-toolbackends: `net_get` + `vault_get` (P0.1)
- **Wat:** in `FsToolBackend::execute` (`kernel/src/agent.rs:121`) `net_get` implementeren via `net::http_fetch`/`fetch_full`, dubbel gegate: capability `NET_GET` **én** de `network_domains`-allow-list uit het manifest (geen domein in de lijst → geweigerd, ook mét cap). `vault_get` via `eurovault::Vault::get` gegate op `VAULT_READ`; het secret gaat **alleen het tool-resultaat van die ene call** in — nooit het transcript/log (north-star 5: credentials at the boundary). `exec` blijft `ERR_CAP_DENIED` (bewust, deny-by-default).
- **Bestanden:** `kernel/src/agent.rs`, `crates/euroagent/src/mcp.rs` (tooldefs `:40`), evt. `crates/euroagent/src/manifest.rs` (domains doorgeven aan de backend).
- **Done:** `[aa-fs]` uitgebreid: net_get-met-cap haalt echt op over EuroNet (SLIRP-mock als peer), zonder cap geweigerd, buiten `network_domains` geweigerd; vault_get met cap levert het secret, zonder cap `EPERM`; audit toont de call maar nooit de waarde. `cargo test -p euroagent` dekt de gating. 0 panics.

### AD-2 — Echt LLM-pad als default, mock eerlijk gelabeld (P0.2)
- **Wat:** dispatch/`run_intent` (`agent.rs:204`, `:495`) probeert eerst `NetOllama` (10.0.2.2:11434); alleen bij onbereikbaarheid terugvallen op `ScriptedLlm` (`:129`) **met `[mock]`-prefix** in transcript + serial ("no local model reachable"). Geen stil mocken meer — dit is regel 1.
- **Bestanden:** `kernel/src/agent.rs`, `kernel/src/agent_ui.rs` (transcript-weergave).
- **Done:** boot mét host-Ollama-mock (kind van de boot-taak) toont een echte round-trip die een toolcall aandrijft (uitbreiding van `[bb1]` end-to-end); boot zónder endpoint toont de duidelijk gelabelde `[mock]`-regel.

### AD-3 — Agent-audit persistent: hash-chain + append-only op EuroFS (P0.3)
- **Wat:** de `AuditRecord`s van de gateway en de EuroID-`AuditLog`-regels serialiseren naar `/var/log/euro/audit.log` met `FLAG_APPEND_ONLY` (hergebruik P3-mechanisme, `kernel/src/audit.rs`); bij boot laden + `verify_chain()`; tamper → geweigerd door FS én gedetecteerd door de keten.
- **Bestanden:** `crates/euroagent/src/mcp.rs`, `crates/euroid/src/audit.rs` (`lines()`/parse), `kernel/src/audit.rs`, `kernel/src/agent.rs`.
- **Done:** nieuw `[xx]`-zelftest: toolcall-audits geschreven → remount (reboot-simulatie) → `verify-chain` slaagt over de geladen keten; FS weigert een verkort/herschreven bestand. Host-test voor de persistentie-round-trip (serialize → parse → verify).

**Sprintdemo (de "kan wat we beloven"-toets):** één boot waarin een agent via het echte model een `net_get` doet binnen zijn domein-allow-list, een `vault_get` met cap (waarde niet in het log), een geweigerde call zonder cap — en na reboot is de hele keten verifieerbaar intact.

---

## Sprint AE — EuroID: soevereine identiteit end-to-end `R 🔒`

**Sprintdoel.** Gebruikersbeheer dat écht onthoudt en écht inlogt: de store overleeft een reboot, en het login-pad van shell + desktop loopt via de from-scratch Argon2id-flow in plaats van de legacy SHA-256.

**Schatting:** 2–3 sessies · **Afhankelijkheden:** geen (EuroFS-flags + euroid-crate bestaan; K1 leverde de kern). · **Backlog:** P1.1, P1.2.

### AE-1 — EuroID-store persistent naar EuroFS (P1.1)
- **Wat:** handmatige (de)serialisatie voor `UserDb`/`GroupDb` in de crate (no_std, géén serde — volg de stijl van audit/superblock-encoders), kernel laadt bij boot en saved bij mutatie naar `/etc/euro/{users,groups,shadow}.db` + `policy.toml`. `shadow.db` → `FLAG_IMMUTABLE` + root-only; users/groups → immutable-by-root.
- **Bestanden:** `crates/euroid/src/model.rs` (+ `cred.rs` voor `PasswordRecord`-encoding), `kernel/src/euroid.rs`.
- **Let op:** echte accounts met de **soevereine Argon2id-params** (64 MiB/t=3/p=4) — `BOOT_PARAMS` blijven exclusief voor de TCG-zelftest, duidelijk gecommentarieerd (regel uit §9).
- **Done:** `eurousers add` → **reboot** → gebruiker bestaat nog (`eurousers list`); `shadow.db` niet schrijfbaar zonder `CAP_IMMUTABLE_ADMIN`; host-tests voor de round-trip (incl. corrupt-bestand → nette fout, geen panic).

### AE-2 — `login`/`su`/`passwd` + desktop op `euroid::authenticate` (P1.2)
- **Wat:** credential-verificatie in `shell.rs` (login/su/sudo/passwd) en de desktop-login routeren door `euroid::authenticate` (Argon2id, timing-attack-preventie, lockout, audit-events verplicht weggeschreven). `auth.rs` reduceren tot dunne sessie-state (uid/gid/naam) of migreren; de SHA-256-verificatie verdwijnt of wordt expliciet als legacy-bridge gemarkeerd.
- **Bestanden:** `kernel/src/shell.rs`, `kernel/src/auth.rs`, desktop-login-handler.
- **Done:** `login alice <pw>` verifieert via Argon2id; 5 foute pogingen vergrendelen het account; elke poging audit; `[k1]`-achtige bootregel bewijst het nieuwe pad; de oude flow is weg of eerlijk gelabeld.

**Sprintdemo:** gebruiker aanmaken, uitloggen, **rebooten**, inloggen met het juiste wachtwoord (Argon2id), vergrendeling na 5 foute pogingen — en alles staat in de audit.

---

## Sprint AF — Zero-Trust-gaten dichten die we adverteren `U 🔒`

**Sprintdoel.** De drie 🟡's uit de mapping die de /zero-trust/-pagina noemt, naar ✅: hardware-gebonden sleutels (PCR-sealing), JIT-elevatie met auto-revoke, en een minimale gedrags-baseline met alerts.

**Schatting:** 3–4 sessies · **Afhankelijkheden:** AD (AF-2/AF-3 bouwen op het echte tool-pad), AE-1 (AF-1 beschermt o.a. de persistente sleutels). · **Backlog:** P2.1, P2.2, P2.3.

### AF-1 — PCR-sealing van EuroVault-master-key + FDE-sleutel (P2.1)
- **Wat:** seal/unseal in `crates/eurotpm` (TPM2_Create/Load/Unseal met PCR-policy, encoder/parser host-getest zoals de bestaande TPM-commando's); kernel: vault-masterkey en FDE-sleutel sealed opslaan; de gereserveerde `kdf_params`/`wrapped_key`-superblok-slots vullen.
- **Bestanden:** `crates/eurotpm/src/lib.rs`, `kernel/src/tpm.rs`, `kernel/src/vault.rs`, FDE-wiring in `kernel/src/main.rs` (`[k3]`), `crates/eurofs/src/superblock.rs` (slots).
- **Risico:** QEMU's TPM-emulatie (swtpm?) — als de sandbox geen sealing-capabele TPM heeft: implementeer + host-test de command-encoding volledig, bewijs in boot wat kan, en label de rest eerlijk 🟡 hardware-attended (zoals WiFi).
- **Done:** `[xx]` bewijst: unseal slaagt bij matchende PCR's en **faalt** na een PCR-extend (verkeerde boot-state); host-tests voor de command-bytes.

### AF-2 — JIT-capability-elevatie + auto-revoke voor agent-taken (P2.2)
- **Wat:** het `needs_confirmation`/elevated-pad in `crates/euroagent/src/policy.rs` operationaliseren: een elevated tool-call krijgt de cap alleen voor díe call (grant vóór, revoke ná, ook bij fout/timeout — RAII-stijl), en grant + revoke worden ge-audit.
- **Bestanden:** `crates/euroagent/src/policy.rs`, `crates/euroagent/src/agentloop.rs`, `kernel/src/agent.rs`.
- **Done:** host-tests: elevated call slaagt mét tijdelijke grant, de cap is wég na de call (een tweede call zonder nieuwe grant faalt), audit bevat grant+revoke; `[xx]`-regel toont de cyclus live.

### AF-3 — Minimale gedrags-baseline + anomaly-hooks (P2.3)
- **Wat:** per-tool tellers/drempels (calls/run, onverwachte tool, datavolume) in `euroobserve`; de agent-loop meet per stap; drempel-overschrijding → ge-audit alert (géén ML — eerlijk Foundation-tier drempelwerk, zoals het framework zelf als instap definieert).
- **Bestanden:** `crates/euroobserve/src/lib.rs`, `crates/euroagent/src/agentloop.rs`, `kernel/src/observe.rs`.
- **Done:** host-tests voor de drempellogica; `[xx]`: een loop die zijn tool-budget overschrijdt triggert een audited alert; metrics zichtbaar via `metrics`.

**Sprintdemo + nazorg:** na AF de statuslabels bijwerken in `ZERO-TRUST-FOR-AI-AGENTS-MAPPING.md` + Appendix B van de deep reference, en (op verzoek) de 🟡→✅-wijzigingen doorvoeren op de /zero-trust/-pagina — claims en code blijven in lockstep.

---

## Sprint AG — Breedte (parallel/optioneel; pas claimen na AD–AF groen) `U`

**Sprintdoel.** Zichtbare gebruikerswaarde naast het security-werk. Items zijn onafhankelijk; kies per sessie. **Schatting:** 1–2 sessies per item. · **Backlog:** P3.

| Item | Wat | Waar | Done |
|---|---|---|---|
| AG-1 EuroApps-GUI's (EuroFiles/EuroNotes/EuroClock) ✅ | `render()`-vensters voor de geverifieerde engines via het `SuiteApp`-dispatchpatroon | `kernel/src/{files,notes,clockapp}.rs`, `compositor.rs`, `suite_ui.rs` | **✅ DONE 2026-06-12** (`[ag]`, screenshots/ag1-desktop.png): EuroFiles=live EuroFS · EuroNotes=euronotes · EuroClock=RTC+wereldklokken; dock 6→8 eerlijke tegels; klik-navigatie/-selectie werkt |
| AG-2 Browser: afbeeldingen + formulieren ✅ | QOI/PPM-decode (euromedia hergebruiken) in de layout/paint; `<input>`/`<form>` met GET-submit | `crates/euroweb/{layout,paint}.rs`, `kernel/src/webview.rs` | **✅ DONE 2026-06-12** (`[ag2]`, screenshots/ag2-browser.png): `<img>` met QOI/PPM-decode (euromedia) rendert; `<input>`/`<form>` → echte GET-submit (`/zoek?q=…`); in-page klik/typen werkt |
| AG-3 Installer-executie ✅ | echte GPT + FAT32-ESP (eigen `eurofat`-crate) + EuroFS naar een 2e virtio-schijf; kernel leest eigen install-media via UEFI (geen embed) | `crates/eurofat`, `kernel/src/instexec.rs`, `scripts/install-test.py` | **✅ DONE 2026-06-12** (`[q1x2]`, screenshots/ag3-standalone.png): kernel installeert bootbare EuroOS naar blanco schijf; die boot STANDALONE; fsck/mtools/sgdisk + host-QEMU-boot gevalideerd |
| AG-4 Coreutils long-tail ✅ | `xargs` (+ `-n N`) als pijplijn-stage + pipe-stdin voor sha224/384sum | `kernel/src/shell.rs` | **✅ DONE 2026-06-12** (`[pipe]` boot-zelftest: `seq 3\|xargs echo`, `seq 4\|xargs -n2 echo`, sha224-filter) |
| AG-5 TTS-verkenning | onderzoek/prototype verstaanbare spraak (formant/diphone) — **earcons blijven het eerlijke verhaal tot dit echt werkt** | `crates/euroaudio`, `kernel/src/access.rs` | expliciet onderzoeksresultaat; géén claim zonder verstaanbare output |

---

## Overkoepelend

- **Definition of done per sprint:** alle taak-DoD's + `cargo test` volledig groen (≥690, nieuwe tests erbij) + één schone boot met alle bestaande `[xx]`-markers ✓ en 0 panics + docs bijgewerkt (deep reference Appendix B, ROADMAP, ZT-mapping) + memory-log.
- **Risico's:** (1) TCG-boots zijn traag (~5 min) — batch wijzigingen per bootrun; (2) TPM-sealing mogelijk hardware-attended in de sandbox (zie AF-1-risico, eerlijk labelen); (3) AE-2 raakt het bestaande login-pad — regressietest `su`/`sudo`/vault-cap-gating (die op `session_uid` leunen) expliciet; (4) website-claims pas bijwerken **nadat** de code groen is, nooit ervoor.
- **Niet in deze cyclus (bewust):** mTLS-pinning als agent-transport, distributed tracing/SIEM-streaming, SEV/TDX, GPU-3D, K4/Q-hardware — zie `docs/ROADMAP.md`.

*Bron: `docs/AGENT-BRIEFING.md` §7 (P0–P3) + `docs/ZERO-TRUST-FOR-AI-AGENTS-MAPPING.md` (gap-lijst). Sprintcodes AD–AG volgen op AA (EuroAgent), AB (EuroBrowser), AC (EuroApps) in `NEXT-SPRINTS.md`.*
