# EuroUpdate — Veilig OS Update Systeem
## Track 9 van het EuroKernel Project
## Technische Specificatie v0.1 & Claude Code Build Prompt

> **Het update systeem is één van de meest kritieke beveiligingscomponenten
> van een OS. Een fout hier kan het hele systeem compromitteren of
> onbruikbaar maken. Correctheid en veiligheid primeren boven snelheid.**
>
> **Kernprincipes:**
> - Een update die mislukt mag NOOIT een onbootbaar systeem achterlaten
> - Elke update is cryptografisch geverifieerd voor installatie
> - Rollback is altijd mogelijk — zelfs na herstart
> - Gebruiker heeft volledige controle en transparantie
> - Geen updates zonder expliciete toestemming (of geconfigureerde policy)
> - Nul telemetrie — geen rapportage naar externe servers
>
> **Afhankelijkheden:**
> - EuroFS snapshots (al aanwezig in Track 2)
> - eupkg signing infrastructuur (al aanwezig in Track 6)
> - EuroGuard permissies (Track 7)
> - A/B partitie schema (nieuw — sectie 3)

---

## 1. Architectuur Overzicht

### 1.1 Het Fundamentele Probleem

Een OS updaten is inherent gevaarlijk:
- De bestanden die je updatet zijn dezelfde bestanden die nu draaien
- Als de update halverwege faalt → mogelijk onbootbaar systeem
- Als de update een bug bevat → mogelijk onbruikbaar systeem
- Als de update vervalst is → mogelijk gecompromitteerd systeem

### 1.2 De Oplossing — Drie Veiligheidslagen

```
Laag 3: Cryptografische verificatie
  → Elke update gesigneerd met Ed25519
  → Verificatie VOOR installatie
  → Geen instalatie zonder geldige handtekening

Laag 2: Atomische updates (A/B systeem)
  → Update gaat naar inactief slot
  → Boot naar nieuw slot pas na volledige installatie
  → Bij probleem: automatische terugval naar oud slot

Laag 1: EuroFS Snapshots
  → Automatische snapshot voor elke update
  → Rollback op bestandsniveau altijd mogelijk
  → Geen data verlies bij mislukte update
```

### 1.3 Update Stroomdiagram

```
Updateserver beschikbaar?
  ↓ ja
Download update metadata (gesigneerd)
  ↓
Verifieer handtekening metadata
  ↓ geldig
Toon gebruiker: versie, changelog, grootte
  ↓ gebruiker akkoord (of auto-policy)
Download update pakket (gesegmenteerd, hervattbaar)
  ↓
Verifieer SHA256 hash van volledig pakket
  ↓ correct
Maak EuroFS snapshot van huidig systeem
  ↓
Installeer update naar INACTIEF A/B slot
  ↓
Verifieer installatie (hash check op doel)
  ↓ OK
Markeer nieuw slot als "pending boot"
  ↓
Herstart (op verzoek gebruiker of gepland)
  ↓
Boot naar nieuw slot
  ↓
Systeem draait? Wacht 5 minuten
  ↓ ja
Markeer nieuw slot als "gezond" (commit)
  ↓
Update voltooid ✓

Bij ELKE fout in bovenstaande stroom:
  → Houd huidig slot actief
  → Verwijder gedeeltelijk geïnstalleerde update
  → Rapporteer fout aan gebruiker
  → Systeem blijft altijd opstartbaar
```

---

## 2. Update Types

### 2.1 Drie Soorten Updates

```
Type 1: Kernel + Systeem Update
  → Bevat nieuwe kernel binary
  → Bevat systeem bibliotheken
  → Bevat systeem configuratie updates
  → VEREIST herstart
  → Geïnstalleerd via A/B slot mechanisme

Type 2: App Update
  → Individuele .eupkg packages
  → Geen herstart nodig
  → Geïnstalleerd via eupkg install --update
  → Kan live vervangen worden als app niet draait

Type 3: Beveiligingsupdate (hotfix)
  → Kritieke kwetsbaarheid fix
  → Kan een subset van systeem zijn
  → Zelfde verificatie als Type 1
  → Gemarkeerd als urgent — andere UX flow
```

### 2.2 Update Kanalen

```toml
# /etc/euroupdated.toml

[channel]
# stable: uitgebreid getest, voor eindgebruikers
# beta:   nieuwere features, voor early adopters
# dev:    dagelijkse builds, voor developers
name = "stable"

[policy]
# auto:    download en installeer automatisch
# notify:  download automatisch, vraag voor installatie
# manual:  alleen notificeer, gebruiker doet alles
mode = "notify"

# Wanneer controleren op updates
check_interval_hours = 24

# Automatisch installeren van beveiligingsupdates
# zelfs als policy = "manual"
auto_security = true

# Herstart automatisch na update (enkel als geen actieve gebruikers)
auto_reboot = false
auto_reboot_time = "03:00"  # Enkel als auto_reboot = true
```

---

## 3. A/B Partitie Schema

### 3.1 Schijfindeling

```
┌─────────────────────────────────────────────┐
│  Partitie 1: EFI System Partition (ESP)     │
│  FAT32, 512 MB                              │
│  /EFI/BOOT/BOOTX64.EFI  ← EuroBoot         │
│  /EFI/eurokernel/boot.cfg ← Boot config    │
├─────────────────────────────────────────────┤
│  Partitie 2: EuroOS-A (EuroFS)              │
│  Systeem slot A — huidig actief             │
│  /boot/kernel.efi                           │
│  /boot/kernel.hash                          │
│  /boot/kernel.sig                           │
│  /usr/  /lib/  /etc/  /bin/                 │
├─────────────────────────────────────────────┤
│  Partitie 3: EuroOS-B (EuroFS)              │
│  Systeem slot B — update target             │
│  (zelfde structuur als A)                   │
├─────────────────────────────────────────────┤
│  Partitie 4: Gebruikersdata (EuroFS)        │
│  NOOIT aangeraakt door OS updates           │
│  /home/  /var/  /opt/                       │
└─────────────────────────────────────────────┘
```

### 3.2 Boot Configuratie

```toml
# /EFI/eurokernel/boot.cfg
# Beheerd door EuroBoot — niet handmatig aanpassen

[boot]
active_slot = "A"          # Huidig actief slot
pending_slot = ""          # Leeg = geen pending update
boot_attempts_remaining = 3 # Reset na succesvolle boot

[slot_a]
device = "/dev/nvme0n1p2"
kernel = "/boot/kernel.efi"
kernel_hash = "sha256:abc123..."
status = "good"            # good | pending | failed

[slot_b]
device = "/dev/nvme0n1p3"
kernel = "/boot/kernel.efi"
kernel_hash = "sha256:def456..."
status = "empty"           # good | pending | failed | empty
```

### 3.3 Boot Logica in EuroBoot

```rust
// bootloader/src/main.rs

#[entry]
fn efi_main(image: Handle, mut st: SystemTable<Boot>) -> Status {
    let config = load_boot_config();

    // Bepaal welk slot te booten
    let boot_slot = determine_boot_slot(&config);

    match boot_slot {
        BootDecision::UseSlot(slot) => {
            // Verminder boot_attempts_remaining
            decrement_attempts(&mut config, slot);
            save_boot_config(&config);

            // Laad en verifieer kernel
            let kernel = load_kernel(slot, &config)?;
            verify_kernel_signature(&kernel, slot, &config)?;

            // Boot kernel
            boot_kernel(kernel, st)
        }
        BootDecision::Fallback(reason) => {
            // Nieuwe slot mislukt — val terug naar vorige
            log!("Boot fallback: {}", reason);
            let fallback = config.inactive_slot();
            mark_slot_failed(&mut config, config.pending_slot());
            save_boot_config(&config);
            boot_kernel_from_slot(fallback, &config, st)
        }
        BootDecision::RecoveryMode => {
            // Beide slots mislukt — boot recovery
            boot_recovery(st)
        }
    }
}

fn determine_boot_slot(config: &BootConfig) -> BootDecision {
    // Pending update?
    if !config.pending_slot.is_empty() {
        let pending = config.pending_slot();

        // Nog pogingen over?
        if config.boot_attempts_remaining > 0 {
            return BootDecision::UseSlot(pending);
        } else {
            // Te veel mislukte pogingen → fallback
            return BootDecision::Fallback("Te veel mislukte bootpogingen");
        }
    }

    // Geen pending — boot actief slot
    let active = config.active_slot();
    if config.slot_status(active) == SlotStatus::Good {
        BootDecision::UseSlot(active)
    } else {
        // Actief slot beschadigd?
        let inactive = config.inactive_slot();
        if config.slot_status(inactive) == SlotStatus::Good {
            BootDecision::Fallback("Actief slot beschadigd")
        } else {
            BootDecision::RecoveryMode
        }
    }
}
```

---

## 4. Update Infrastructuur

### 4.1 Update Server Protocol

```
GET https://updates.euro-os.eu/api/v1/check
Headers:
  X-EuroOS-Version: 0.1.0
  X-EuroOS-Channel: stable
  X-EuroOS-Arch: x86_64
  X-EuroOS-Build: 20260601

Response (gesigneerde JSON):
{
  "latest_version": "0.2.0",
  "build": "20260615",
  "is_security": false,
  "size_bytes": 245678901,
  "sha256": "abc123...",
  "signature": "ed25519:base64...",
  "signing_key_id": "eurokernel-release-2026",
  "download_url": "https://updates.euro-os.eu/releases/0.2.0/update.eurupdate",
  "delta_available": true,
  "delta_url": "...",
  "delta_size_bytes": 12345678,
  "changelog_url": "https://updates.euro-os.eu/changelog/0.2.0",
  "changelog_hash": "sha256:..."
}
```

### 4.2 Update Pakket Formaat (.eurupdate)

```
update.eurupdate (gecomprimeerd archief):
├── MANIFEST.toml          # Metadata en handtekeningen
├── signature.ed25519      # Handtekening van MANIFEST
├── kernel/
│   ├── kernel.efi         # Nieuwe kernel binary
│   ├── kernel.efi.sig     # Kernel handtekening
│   └── kernel.efi.hash    # SHA256 van kernel
├── system/
│   ├── files.tar.zst      # Gecomprimeerde systeembestanden
│   ├── files.hash         # SHA256 van archief
│   └── files.manifest     # Lijst van alle bestanden + hashes
├── scripts/
│   ├── pre-install.sh     # Optioneel: voor installatie
│   ├── post-install.sh    # Optioneel: na installatie
│   └── migrate.sh         # Configuratie migratie
└── delta/                 # Optioneel: delta update
    ├── patches/           # bsdiff patches per bestand
    └── delta.manifest     # Welke bestanden gewijzigd zijn
```

```toml
# MANIFEST.toml binnen .eurupdate

[update]
version = "0.2.0"
previous_version = "0.1.0"   # Welke versie dit update
build_date = "2026-06-15T00:00:00Z"
channel = "stable"
is_security_update = false
requires_reboot = true

[signing]
key_id = "eurokernel-release-2026"
algorithm = "ed25519"
# Handtekening staat in signature.ed25519

[hashes]
kernel = "sha256:abc123..."
system_files = "sha256:def456..."
manifest_self = "sha256:ghi789..."

[compatibility]
min_version = "0.1.0"        # Minimum vorige versie voor deze update
arch = ["x86_64"]
```

### 4.3 Delta Updates

Delta updates bevatten alleen gewijzigde bestanden — veel kleiner:

```rust
// userland/euroupdated/src/delta.rs

/// Pas een delta patch toe op een bestaand bestand
/// Gebruikt bsdiff/bspatch formaat — bewezen en efficient
pub fn apply_delta_patch(
    old_file: &Path,
    patch_file: &Path,
    new_file: &Path,
) -> Result<(), DeltaError> {
    let old_data = fs::read(old_file)?;
    let patch_data = fs::read(patch_file)?;

    // Verifieer patch hash voor toepassing
    let patch_hash = sha256(&patch_data);
    let expected = read_expected_hash(patch_file)?;
    if patch_hash != expected {
        return Err(DeltaError::PatchCorrupted);
    }

    // Pas bspatch toe
    let new_data = bspatch::patch(&old_data, &patch_data)?;

    // Schrijf naar tijdelijk bestand eerst
    let tmp = new_file.with_extension(".tmp");
    fs::write(&tmp, &new_data)?;

    // Verifieer resultaat
    let result_hash = sha256(&new_data);
    let expected_result = read_expected_result_hash(patch_file)?;
    if result_hash != expected_result {
        fs::remove_file(&tmp)?;
        return Err(DeltaError::PatchResultInvalid);
    }

    // Atomisch vervangen
    fs::rename(&tmp, new_file)?;
    Ok(())
}
```

---

## 5. euroupdated — Update Daemon

### 5.1 Daemon Structuur

```rust
// userland/euroupdated/src/main.rs

/// EuroUpdate daemon — draait als systeemdienst
/// Beheert het volledige update lifecycle

pub struct UpdateDaemon {
    config:          UpdateConfig,
    state:           UpdateState,
    http_client:     HttpClient,
    signer:          SignatureVerifier,
    storage:         UpdateStorage,
    notifier:        UserNotifier,
    ipc:             IpcServer,          // Communiceert met eupdate CLI/GUI
}

#[derive(Debug, Clone)]
pub enum UpdateState {
    Idle,
    Checking,
    Downloading { progress: f32, bytes_total: u64, bytes_done: u64 },
    Verifying,
    Installing { phase: InstallPhase },
    PendingReboot { version: String },
    Failed { reason: String, recoverable: bool },
}

#[derive(Debug, Clone)]
pub enum InstallPhase {
    CreatingSnapshot,
    WritingKernel,
    WritingSystemFiles,
    VerifyingInstall,
    UpdatingBootConfig,
}

impl UpdateDaemon {
    pub fn run(&mut self) -> ! {
        loop {
            match self.state.clone() {
                UpdateState::Idle => {
                    if self.should_check_now() {
                        self.check_for_updates();
                    }
                    sleep_ms(60_000); // Check elke minuut of het tijd is
                }
                UpdateState::Downloading { .. } => {
                    self.continue_download();
                }
                _ => {
                    // Wacht op IPC commando's
                    self.handle_ipc_messages();
                }
            }
        }
    }

    /// Controleer of er een update beschikbaar is
    fn check_for_updates(&mut self) {
        self.state = UpdateState::Checking;
        self.notifier.notify_state_change(&self.state);

        match self.fetch_update_metadata() {
            Ok(Some(meta)) => {
                // Update beschikbaar
                kinfo!("update", &alloc::format!(
                    "Update beschikbaar: {} → {}", current_version(), meta.version
                ));

                // Notificeer gebruiker
                self.notifier.notify_update_available(&meta);

                // Auto-download als geconfigureerd
                if self.config.policy == UpdatePolicy::Auto
                    || (self.config.auto_security && meta.is_security)
                {
                    self.start_download(meta);
                }
            }
            Ok(None) => {
                // Geen update
                self.state = UpdateState::Idle;
            }
            Err(e) => {
                kwarn!("update", &alloc::format!("Update check mislukt: {:?}", e));
                self.state = UpdateState::Idle;
                // Probeer later opnieuw — geen foutmelding aan gebruiker
                // tenzij meerdere opeenvolgende fouten
            }
        }
    }

    /// Download update pakket — hervattbaar
    fn start_download(&mut self, meta: UpdateMetadata) {
        // Kies delta of volledig pakket
        let (url, expected_size) = if meta.delta_available
            && self.has_correct_base_version(&meta)
        {
            (meta.delta_url.clone(), meta.delta_size_bytes)
        } else {
            (meta.download_url.clone(), meta.size_bytes)
        };

        self.state = UpdateState::Downloading {
            progress: 0.0,
            bytes_total: expected_size,
            bytes_done: 0,
        };

        // Download met hervatbaarheid
        // Als daemon herstart tijdens download → hervat vanaf onderbroken punt
        let tmp_path = self.storage.download_path(&meta.version);
        self.http_client.download_resumable(&url, &tmp_path, |done, total| {
            self.state = UpdateState::Downloading {
                progress: done as f32 / total as f32,
                bytes_total: total,
                bytes_done: done,
            };
            self.notifier.notify_state_change(&self.state);
        });
    }

    /// Installeer een gedownloade update
    fn install_update(&mut self, meta: UpdateMetadata) -> Result<(), UpdateError> {
        // Stap 1: Verifieer handtekening
        self.state = UpdateState::Verifying;
        self.verify_update_package(&meta)?;

        // Stap 2: Maak snapshot
        self.state = UpdateState::Installing {
            phase: InstallPhase::CreatingSnapshot
        };
        let snapshot_id = EUROFS.create_snapshot(
            &alloc::format!("pre-update-{}", meta.version)
        )?;
        kinfo!("update", &alloc::format!(
            "Snapshot aangemaakt: {}", snapshot_id
        ));

        // Stap 3: Schrijf naar inactief slot
        let target_slot = BOOT_CONFIG.inactive_slot();

        self.state = UpdateState::Installing {
            phase: InstallPhase::WritingKernel
        };
        self.write_kernel_to_slot(target_slot, &meta)?;

        self.state = UpdateState::Installing {
            phase: InstallPhase::WritingSystemFiles
        };
        self.write_system_files_to_slot(target_slot, &meta)?;

        // Stap 4: Verifieer installatie
        self.state = UpdateState::Installing {
            phase: InstallPhase::VerifyingInstall
        };
        self.verify_installed_slot(target_slot, &meta)?;

        // Stap 5: Update boot config (ATOMISCH)
        self.state = UpdateState::Installing {
            phase: InstallPhase::UpdatingBootConfig
        };
        BOOT_CONFIG.set_pending_slot(target_slot);
        BOOT_CONFIG.set_boot_attempts(3);
        BOOT_CONFIG.save_atomic()?; // Atomische write — interrupts hier zijn veilig

        // Klaar
        self.state = UpdateState::PendingReboot {
            version: meta.version.clone()
        };
        self.notifier.notify_reboot_required(&meta.version);

        kinfo!("update", &alloc::format!(
            "Update {} geïnstalleerd — herstart vereist", meta.version
        ));

        Ok(())
    }

    /// Verifieer cryptografische handtekening van update pakket
    fn verify_update_package(&self, meta: &UpdateMetadata) -> Result<(), UpdateError> {
        let package_path = self.storage.download_path(&meta.version);

        // 1. Lees MANIFEST.toml uit pakket
        let manifest = read_manifest_from_package(&package_path)?;

        // 2. Verifieer handtekening van MANIFEST
        let sig = read_signature_from_package(&package_path)?;
        let pubkey = self.signer.get_key(&manifest.signing.key_id)
            .ok_or(UpdateError::UnknownSigningKey)?;

        if !verify_ed25519(&sig, manifest.as_bytes(), pubkey) {
            return Err(UpdateError::InvalidSignature);
        }

        // 3. Verifieer hash van volledig pakket
        let package_hash = sha256_file(&package_path)?;
        if package_hash != meta.sha256 {
            return Err(UpdateError::HashMismatch);
        }

        // 4. Verifieer kernel handtekening apart
        let kernel = extract_kernel_from_package(&package_path)?;
        let kernel_sig = extract_kernel_sig_from_package(&package_path)?;
        if !verify_ed25519(&kernel_sig, &kernel, pubkey) {
            return Err(UpdateError::KernelSignatureInvalid);
        }

        kinfo!("update", "Pakket verificatie geslaagd");
        Ok(())
    }
}
```

### 5.2 Boot Confirmatie na Update

```rust
// kernel/src/update/confirm.rs
// Geroepen door init systeem na succesvolle boot

/// Bevestig dat de nieuwe versie correct draait
/// Geroepen door euroinit 5 minuten na opstarten
pub fn confirm_successful_boot() {
    let config = BOOT_CONFIG.read();

    if config.pending_slot.is_empty() {
        return; // Geen pending update
    }

    let pending = config.pending_slot();

    // Systeem draait — update is succesvol
    kinfo!("update", &alloc::format!(
        "Update succesvol bevestigd — slot {} is nu actief", pending
    ));

    BOOT_CONFIG.update(|cfg| {
        cfg.active_slot = pending;
        cfg.pending_slot = String::new();
        cfg.set_slot_status(pending, SlotStatus::Good);
        cfg.boot_attempts_remaining = 3; // Reset voor volgende update
    });

    // Ruim oud slot op (optioneel — houd het als extra backup)
    // BOOT_CONFIG.clear_slot(config.inactive_slot());

    // Notificeer gebruiker
    NOTIFIER.send(Notification {
        title: "Update voltooid",
        body: &alloc::format!("EuroOS {} is succesvol geïnstalleerd", current_version()),
        icon: NotificationIcon::Success,
        actions: vec![
            NotificationAction { id: "changelog", label: "Bekijk wijzigingen" },
        ],
    });
}

/// Rollback naar vorige versie
/// Kan geroepen worden door gebruiker of automatisch na boot-fout
pub fn rollback_to_previous() -> Result<(), RollbackError> {
    let config = BOOT_CONFIG.read();
    let previous = config.inactive_slot();

    if config.slot_status(previous) != SlotStatus::Good {
        return Err(RollbackError::NoPreviousGoodSlot);
    }

    kinfo!("update", &alloc::format!(
        "Rollback naar slot {}", previous
    ));

    BOOT_CONFIG.update(|cfg| {
        cfg.pending_slot = previous.to_string();
        cfg.boot_attempts_remaining = 1; // Één poging voor rollback
    });

    Ok(())
    // Gebruiker moet nog herstarten
}
```

---

## 6. Signing Infrastructuur

### 6.1 Sleutelhiërarchie

```
EuroOS Root CA (offline, HSM)
  │
  ├── Kernel Signing Key (online, HSM)
  │     Gebruikt voor: kernel.efi, kernel.sig
  │
  ├── System Update Key (online, HSM)
  │     Gebruikt voor: MANIFEST.toml, update pakketten
  │
  └── Package Signing Key (online, HSM)
        Gebruikt voor: .eupkg packages in EuroStore

Root CA:
  - Nooit online verbonden
  - Opgeslagen op twee HSM's op aparte locaties
  - Wordt gebruikt om sub-sleutels te tekenen
  - Jaarlijkse rotatie van sub-sleutels

Sub-sleutels:
  - Op HSM (bijv. Nitrokey HSM 2)
  - Handtekening vereist fysieke aanwezigheid (PIN)
  - Automatische signing via HSM API voor CI/CD
  - Rotatie elke 6-12 maanden
```

### 6.2 Publieke Sleutels Ingebakken in Kernel

```rust
// kernel/src/update/keys.rs

/// Publieke sleutels voor update verificatie
/// Ingebakken in de kernel — kunnen niet vervangen worden door een update
/// (een update zou de kernel vervangen die deze sleutels bevat)

/// Primaire kernel signing sleutel 2026
pub const KERNEL_SIGNING_KEY_2026: &[u8] = include_bytes!("../keys/kernel-signing-2026.pub");

/// Backup kernel signing sleutel (voor sleutelrotatie)
pub const KERNEL_SIGNING_KEY_2026_BACKUP: &[u8] = include_bytes!("../keys/kernel-signing-2026-backup.pub");

/// Systeem update sleutel
pub const SYSTEM_UPDATE_KEY_2026: &[u8] = include_bytes!("../keys/system-update-2026.pub");

/// Controleer of een handtekening geldig is voor een van onze sleutels
pub fn verify_update_signature(data: &[u8], signature: &[u8], key_id: &str) -> bool {
    let pubkey = match key_id {
        "kernel-signing-2026"        => KERNEL_SIGNING_KEY_2026,
        "kernel-signing-2026-backup" => KERNEL_SIGNING_KEY_2026_BACKUP,
        "system-update-2026"         => SYSTEM_UPDATE_KEY_2026,
        _ => {
            kwarn!("update", &alloc::format!("Onbekende sleutel-ID: {}", key_id));
            return false;
        }
    };

    // Ed25519 verificatie
    ed25519_verify(pubkey, data, signature)
}
```

### 6.3 Sleutelrotatie Procedure

```
Wanneer: Jaarlijks of bij verdachte compromittering

Procedure:
1. Genereer nieuw sleutelpaar op offline HSM
2. Teken nieuwe publieke sleutel met Root CA
3. Publiceer nieuwe sleutel op euro-os.eu/keys/
4. Bouw nieuwe kernel die BEIDE sleutels bevat (oud + nieuw)
5. Lever kernel update met beide sleutels
6. Na X maanden: lever update die oude sleutel verwijdert
7. Intrek oude sleutel publiek op Certificate Transparency log

Emergency revocatie (bij compromittering):
1. Publiceer revocatie op euro-os.eu/security/
2. Push urgente update via alle kanalen
3. Revocatie lijst ingebakken in volgende kernel update
4. Gebruikers die niet updaten: waarschuwing bij elke boot
```

---

## 7. eupdate CLI & GUI

### 7.1 eupdate CLI

```bash
# Status opvragen
eupdate status
# Output:
# EuroOS versie: 0.1.0 (build 20260601)
# Kanaal: stable
# Laatste check: 2 uur geleden
# Status: Bijgewerkt ✓

# Handmatig controleren op updates
eupdate check
# Output:
# Controleren op updates...
# Update beschikbaar: 0.2.0
# Type: Systeem update (herstart vereist)
# Grootte: 234 MB (of 12 MB delta)
# Beveiligingsupdate: Nee
# [eupdate download] om te downloaden

# Update downloaden
eupdate download
# Output:
# Downloading 0.2.0... [████████░░] 78% (182/234 MB)
# ETA: 2 minuten

# Update installeren (na download)
eupdate install
# Output:
# Handtekening verificeren... ✓
# Snapshot aanmaken... ✓
# Kernel installeren... ✓
# Systeembestanden installeren... ✓
# Installatie verificeren... ✓
# Boot configuratie bijwerken... ✓
#
# Update 0.2.0 geïnstalleerd.
# Herstart vereist om update te activeren.
# [eupdate reboot] of herstart handmatig

# Alles in één stap
eupdate upgrade
# Controleert, downloadt, installeert, vraagt om herstart

# Herstart na update
eupdate reboot

# Rollback naar vorige versie
eupdate rollback
# Output:
# Vorige versie: 0.1.0 (slot B, status: good)
# Rollback instellen... ✓
# Herstart om terug te keren naar 0.1.0
# [eupdate reboot]

# Updategeschiedenis
eupdate history
# Output:
# 2026-06-15  0.2.0  Geïnstalleerd (actief)
# 2026-06-01  0.1.0  Geïnstalleerd (beschikbaar voor rollback)
# 2026-05-15  0.0.9  Verwijderd (slot hergebruikt)

# Changelog bekijken
eupdate changelog 0.2.0

# Kanaal wijzigen
eupdate channel beta

# Policy instellen
eupdate policy auto     # Automatisch alles
eupdate policy notify   # Vraag voor installatie
eupdate policy manual   # Alleen notificeren
```

### 7.2 Update GUI in EuroSettings

```
┌─────────────────────────────────────────────────────────────┐
│  ⚙️ EuroSettings — Software-updates                         │
│                                                             │
│  EuroOS versie                                              │
│  ──────────────────────────────────────────────────────     │
│  Huidige versie:  0.1.0 (build 20260601)                    │
│  Kanaal:          Stabiel ▾                                 │
│  Laatste check:   Vandaag 08:14                             │
│                                                             │
│  ┌──────────────────────────────────────────────────────┐   │
│  │  🔔 Update beschikbaar: EuroOS 0.2.0                 │   │
│  │                                                      │   │
│  │  Grootte: 12 MB (delta update)                       │   │
│  │  Type: Systeem update · Herstart vereist             │   │
│  │                                                      │   │
│  │  [Changelog bekijken]  [Nu downloaden]               │   │
│  └──────────────────────────────────────────────────────┘   │
│                                                             │
│  Update instellingen                                        │
│  ──────────────────────────────────────────────────────     │
│  Updates controleren:   Dagelijks ▾                         │
│  Installatiemodus:                                          │
│    ○ Automatisch installeren                                │
│    ● Melden, ik installeer zelf          ← aanbevolen       │
│    ○ Alleen melden                                          │
│                                                             │
│  ✓ Beveiligingsupdates altijd automatisch                   │
│  ○ Automatisch herstarten (om 03:00)                        │
│                                                             │
│  Versiegeschiedenis                                         │
│  ──────────────────────────────────────────────────────     │
│  0.1.0  ●  Huidig actief                                    │
│  0.0.9  ○  Beschikbaar voor rollback    [Rollback]          │
│                                                             │
│  [Handmatig controleren]                                    │
└─────────────────────────────────────────────────────────────┘
```

### 7.3 Update Voortgang Scherm

```
┌─────────────────────────────────────────────────────────────┐
│  Update installeren — EuroOS 0.2.0                          │
│                                                             │
│  ✓ Handtekening geverifieerd                                │
│  ✓ Snapshot aangemaakt (pre-update-0.2.0)                   │
│  → Systeembestanden installeren...                          │
│    [████████████████░░░░░░░░] 67%                           │
│                                                             │
│  ℹ️  Uw huidige systeem blijft volledig werkend              │
│     tot u herstart. Rollback is altijd mogelijk.            │
│                                                             │
│  Details                                                    │
│  ──────────────────────────────────────────────────────     │
│  Target slot:     B                                         │
│  Vorige versie:   0.1.0 (blijft beschikbaar op slot A)      │
│  Snapshot:        pre-update-0.2.0 (EuroFS)                 │
│                                                             │
│                                        [Annuleren]          │
└─────────────────────────────────────────────────────────────┘
```

---

## 8. Veiligheidsoverwegingen

### 8.1 Update Server Compromittering

**Scenario:** Aanvaller compromitteert de update server.

**Bescherming:**
- Elke update is gesigneerd met offline sleutel (HSM)
- Aanvaller kan geen valide handtekening produceren zonder HSM
- Kernel weigert updates zonder geldige handtekening
- Certificate Transparency log detecteert valse sleutels

**Wat aanvaller WEL kan doen:**
- Updates achterhouden (denial of update)
- Oude updates opnieuw aanbieden (downgrade attack)

**Aanvullende bescherming tegen downgrade:**
- Versienummer in MANIFEST — kernel weigert oudere versie
- Monotone versienummer counter opgeslagen in TPM
- Update manifest bevat minimum acceptabele versie

### 8.2 Man-in-the-Middle Aanval

**Scenario:** Aanvaller onderschept verbinding met updateserver.

**Bescherming:**
- HTTPS met certificate pinning voor euro-os.eu
- Handtekening verificatie onafhankelijk van transportlaag
- Zelfs bij gecompromitteerde TLS → handtekening beschermt

### 8.3 Rollback Aanval

**Scenario:** Aanvaller dwingt systeem terug naar oude kwetsbare versie.

**Bescherming:**
- Rollback enkel naar vorige EuroOS versie (één stap)
- Rollback vereist kernel boot → kernel verifieert rollback slot
- TPM kan minimum versie nummer afdwingen (anti-rollback)
- Rollback wordt gelogd in audit trail

### 8.4 Update tijdens Actieve Gebruik

**Scenario:** Update installeert terwijl bestanden in gebruik zijn.

**Bescherming:**
- Update gaat naar INACTIEF slot — actief systeem ongewijzigd
- Geen bestanden worden vervangen terwijl ze gebruikt worden
- Na herstart: nieuw slot actief, oud slot intact
- Gebruikersdata nooit aangeraakt

### 8.5 Stroomuitval tijdens Update

**Scenario:** Stroom valt uit tijdens update installatie.

**Bescherming:**
- EuroFS CoW — gedeeltelijke writes beschadigen nooit bestaand slot
- Boot config update is atomisch — of volledig of niet
- Na herstart: boot config ongewijzigd → start van actief slot
- Gedeeltelijk geïnstalleerde update wordt gedetecteerd en verwijderd

### 8.6 Corrupt Update Pakket

**Scenario:** Netwerkfout veroorzaakt beschadigd gedownload pakket.

**Bescherming:**
- SHA256 hash verificatie van volledig pakket voor installatie
- Per-bestand hashes in files.manifest
- Gedeeltelijke download hervattbaar — corrupt segment opnieuw downloaden
- Installatie start nooit met mismatching hash

---

## 9. Roadmap & Budget Track 9

| Fase | Inhoud | Prioriteit | Budget |
|---|---|---|---|
| 9.1 | A/B partitie schema + EuroBoot update logica | 🔴 Kritiek | €60.000 |
| 9.2 | Boot confirmatie + automatische rollback | 🔴 Kritiek | €45.000 |
| 9.3 | euroupdated daemon basis | 🟡 Hoog | €90.000 |
| 9.4 | Update download + verificatie | 🟡 Hoog | €75.000 |
| 9.5 | Installatie naar inactief slot | 🟡 Hoog | €90.000 |
| 9.6 | Signing infrastructuur + key management | 🟡 Hoog | €60.000 |
| 9.7 | eupdate CLI | 🟡 Hoog | €45.000 |
| 9.8 | Update GUI in EuroSettings | 🟢 Medium | €60.000 |
| 9.9 | Delta updates | 🟢 Medium | €75.000 |
| 9.10 | Update server infrastructuur | 🟡 Hoog | €90.000 |
| 9.11 | App updates via eupkg | 🟢 Medium | €45.000 |
| 9.12 | Recovery modus | 🟢 Medium | €60.000 |
| **Totaal** | | | **€795.000** |

---

## 10. Claude Code Build Prompt — Fase 9.1: A/B Boot Schema

> **Geef sectie 10 aan Claude Code.**
> Start dit vroeg — het A/B schema bepaalt de schijfindeling
> en moet aanwezig zijn voor de eerste hardware release.

### Projectstructuur

```
bootloader/
├── Cargo.toml
├── rust-toolchain.toml      # Zelfde als kernel
└── src/
    ├── main.rs              # UEFI entry point EuroBoot
    ├── config.rs            # Boot config lezen/schrijven
    ├── slot.rs              # Slot management (A/B)
    ├── verify.rs            # Kernel handtekening verificatie
    └── recovery.rs          # Recovery modus

userland/euroupdated/
├── Cargo.toml
└── src/
    ├── main.rs              # Daemon entry point
    ├── check.rs             # Update beschikbaarheid
    ├── download.rs          # Herstelbare download
    ├── verify.rs            # Pakket verificatie
    ├── install.rs           # Installatie naar slot
    ├── config.rs            # Update configuratie
    ├── notify.rs            # Gebruikersnotificaties
    └── ipc.rs               # Communicatie met CLI/GUI

userland/eupdate/
├── Cargo.toml
└── src/
    └── main.rs              # CLI tool
```

### Boot Config Formaat

```rust
// bootloader/src/config.rs

/// Boot configuratie — opgeslagen op ESP partitie
/// /EFI/eurokernel/boot.cfg

#[derive(Debug, Serialize, Deserialize)]
pub struct BootConfig {
    pub version:                  u32,         // Config schema versie
    pub active_slot:              Slot,
    pub pending_slot:             Option<Slot>,
    pub boot_attempts_remaining:  u8,

    pub slot_a: SlotConfig,
    pub slot_b: SlotConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Slot { A, B }

#[derive(Debug, Serialize, Deserialize)]
pub struct SlotConfig {
    pub status:       SlotStatus,
    pub version:      String,
    pub kernel_hash:  String,      // SHA256 van kernel.efi
    pub installed_at: u64,         // Unix timestamp
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SlotStatus {
    Good,      // Bevestigd werkend
    Pending,   // Geïnstalleerd, nog niet getest
    Failed,    // Boot mislukt
    Empty,     // Geen OS geïnstalleerd
}

impl BootConfig {
    /// Laad vanuit ESP partitie
    pub fn load(esp: &mut EspPartition) -> Result<Self, ConfigError> {
        let data = esp.read_file("/EFI/eurokernel/boot.cfg")?;
        let config: Self = toml::from_slice(&data)?;
        config.validate()?;
        Ok(config)
    }

    /// Sla atomisch op — schrijf naar .tmp dan rename
    /// Voorkomt corrupt config bij stroomuitval
    pub fn save_atomic(&self, esp: &mut EspPartition) -> Result<(), ConfigError> {
        let data = toml::to_vec(self)?;

        // Schrijf naar tijdelijk bestand
        esp.write_file("/EFI/eurokernel/boot.cfg.tmp", &data)?;
        esp.flush()?; // Zorg dat data op schijf staat

        // Atomisch rename (FAT32 ondersteunt dit)
        esp.rename(
            "/EFI/eurokernel/boot.cfg.tmp",
            "/EFI/eurokernel/boot.cfg"
        )?;
        esp.flush()?;

        Ok(())
    }

    pub fn inactive_slot(&self) -> Slot {
        match self.active_slot {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }
}
```

### EuroBoot Main

```rust
// bootloader/src/main.rs

#[entry]
fn efi_main(image: Handle, mut st: SystemTable<Boot>) -> Status {
    uefi_services::init(&mut st).unwrap();

    let bs = st.boot_services();

    // Laad boot configuratie van ESP
    let esp = find_esp_partition(bs).expect("ESP partitie niet gevonden");
    let mut config = BootConfig::load(&esp).unwrap_or_else(|_| {
        // Eerste boot of config corrupt
        BootConfig::default_first_boot()
    });

    // Bepaal welk slot te booten
    let boot_slot = determine_boot_slot(&config);
    let slot_cfg = config.slot_config(boot_slot);

    // Verminder boot attempt counter
    if config.pending_slot == Some(boot_slot) {
        config.boot_attempts_remaining -= 1;
        config.save_atomic(&esp).expect("Kon boot config niet opslaan");
    }

    // Laad kernel van het gekozen slot
    let kernel_path = slot_device_path(boot_slot, "/boot/kernel.efi");
    let kernel_data = load_file(bs, &kernel_path).expect("Kernel laden mislukt");

    // Verifieer kernel handtekening
    verify_kernel(&kernel_data, &slot_cfg.kernel_hash)
        .expect("Kernel verificatie mislukt — mogelijke manipulatie");

    // Geef informatie door aan kernel
    let boot_params = KernelBootParams {
        active_slot: boot_slot,
        version: slot_cfg.version.clone(),
        is_update_boot: config.pending_slot.is_some(),
    };

    // Boot kernel
    boot_loaded_kernel(kernel_data, boot_params, st)
}
```

---

## 11. Bedenkingen & Valkuilen

### FAT32 Atomische Writes

FAT32 (de ESP partitie) ondersteunt geen echte atomische writes.
De .tmp → rename truc werkt in de meeste gevallen maar is niet
100% gegarandeerd bij stroomuitval midden in de rename operatie.

Oplossing: houd twee kopieën van boot.cfg (boot.cfg en boot.cfg.bak).
Lees altijd beide en gebruik de meest recente geldige versie.

### Boot Attempt Counter Race

Als de kernel start maar crasht voor de confirmatie daemon draait,
wordt de boot attempt counter verlaagd maar nooit hersteld. Na drie
crashes → automatische rollback.

Dit is het gewenste gedrag maar zorg dat de confirmatie vroeg
genoeg in het boot proces plaatsvindt — voor de gebruiker iets ziet
maar na genoeg initialisatie om te weten dat het systeem stabiel is.
Aanbeveling: 60 seconden na eerste gebruikerssessie start.

### Update Server Availability

Wat als de update server niet bereikbaar is?

- Geen update beschikbaar melding — geen foutmelding
- Exponential backoff voor retry
- Lokale cache van laatste bekende versie
- Updates kunnen ook via USB aangeboden worden (air-gap scenario)

### Schijfruimte voor A/B

A/B schema vereist twee keer de schijfruimte voor het OS.
Op een 256 GB SSD is dit acceptabel (OS typisch 4-8 GB per slot).
Op zeer kleine schijven kan dit een probleem zijn.

Oplossing: maak slots kleiner via EuroFS deduplicatie — bestanden
die identiek zijn in beide slots worden maar één keer opgeslagen
(CoW block sharing). Gedeelde ongewijzigde bestanden nemen geen
extra ruimte in.

### App Updates vs OS Updates

App updates (.eupkg) hebben een ander traject dan OS updates.
Apps kunnen live bijgewerkt worden als de app niet draait.
Als de app draait: update wordt gepland voor de volgende keer
dat de app sluit (zoals browser updates op desktop systemen).

OS updates vereisen altijd een herstart omdat kernel en
systeem bibliotheken niet vervangen kunnen worden terwijl ze actief zijn.
