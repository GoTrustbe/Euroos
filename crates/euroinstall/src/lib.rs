//! EuroInstall — de **planner** voor de begeleide installatie + live-image (plan Q1).
//!
//! De installer is bewust opgesplitst: de *beslissingslogica* (partitielayout,
//! validatie, het geordende stappenplan) is pure, host-geteste `no_std`-code; de
//! *uitvoering* (echte sector-I/O via `gpt`/`eurofs`, FDE-enrol via `eurofde`,
//! gebruiker via `auth`) koppelt de kernel/het userspace-installerproces eraan.
//! Zo is het breekbare deel — de schijflayout — volledig te testen zonder schijf.
//!
//! Twee modi: een echte **installatie** naar schijf (A/B-slots + EuroVar + ESP) en
//! een **live**-boot die volledig in RAM draait en de schijf onaangeroerd laat.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

const SECTOR: u64 = 512;
const MIB: u64 = 1024 * 1024;
/// ESP (EFI System Partition) — FAT32 met de loader + A/B-kernels.
const ESP_BYTES: u64 = 256 * MIB;
/// Minimale grootte per systeem-slot.
const SLOT_MIN_BYTES: u64 = 512 * MIB;
/// Minimale EuroVar (schrijfbare data).
const VAR_MIN_BYTES: u64 = 256 * MIB;

/// De geometrie van de doelschijf.
#[derive(Clone, Copy, Debug)]
pub struct Disk {
    pub total_bytes: u64,
}

/// De keuzes die de gebruiker (of de unattended-config) maakt.
#[derive(Clone, Debug)]
pub struct Config {
    pub disk: Disk,
    /// BCP-47-taal-tag, bv. `"nl-BE"` — moet een geldige EU-taal zijn.
    pub locale: String,
    /// Toetsenbordindeling, bv. `"be-azerty"`.
    pub keymap: String,
    pub hostname: String,
    pub username: String,
    /// Full-disk-encryptie inschakelen (EuroFDE, sleutel via TPM).
    pub fde: bool,
    /// Live-modus: niets naar schijf schrijven, alles in RAM.
    pub live: bool,
}

/// Een geplande GPT-partitie (in sectoren van 512 bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionPlan {
    pub label: &'static str,
    pub start_lba: u64,
    pub sectors: u64,
}

/// Eén stap in het installatieplan, in uitvoeringsvolgorde.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Schrijf de GPT met deze partities.
    Partition(Vec<PartitionPlan>),
    /// Formatteer de ESP (FAT32).
    FormatEsp,
    /// Leg EuroFS aan op slot A (en kopieer naar B).
    FormatSystem,
    /// Leg EuroFS aan op de EuroVar-partitie.
    FormatVar,
    /// Schrijf het kernel-image naar slot A én B.
    WriteKernelSlots,
    /// Installeer de twee-traps-loader op de ESP.
    InstallLoader,
    /// FDE inschakelen (ChaCha20, sleutel verzegeld aan de TPM).
    EnrollFde,
    /// Stel de locale in (gebruikt EuroLocale).
    ConfigureLocale(String),
    /// Stel de toetsenbordindeling in.
    ConfigureKeymap(String),
    /// Zet de hostnaam.
    SetHostname(String),
    /// Maak de eerste gebruiker (met sudo).
    CreateUser(String),
    /// Provisioneer de EuroCA (lokale certificaatautoriteit).
    ProvisionEuroCa,
    /// Schrijf de A/B-bootconfig + activeer slot A.
    FinalizeBoot,
}

/// Waarom een config geweigerd wordt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// De schijf is te klein voor een A/B-installatie.
    DiskTooSmall { need_bytes: u64, have_bytes: u64 },
    /// Ongeldige gebruikersnaam (leeg, te lang, of niet [a-z0-9_-]).
    BadUsername,
    /// Lege hostnaam.
    BadHostname,
    /// Onbekende taal-tag (geen EU-taal).
    BadLocale,
    /// Lege keymap.
    BadKeymap,
}

/// Het minimaal vereiste aantal bytes voor een installatie naar schijf.
pub fn minimum_disk_bytes() -> u64 {
    ESP_BYTES + 2 * SLOT_MIN_BYTES + VAR_MIN_BYTES
}

fn valid_username(u: &str) -> bool {
    !u.is_empty()
        && u.len() <= 32
        && u.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        && u.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
}

/// Valideer een config zonder een plan te bouwen.
pub fn validate(cfg: &Config) -> Result<(), PlanError> {
    if eurolocale::Lang::parse(&cfg.locale).is_none() {
        return Err(PlanError::BadLocale);
    }
    if cfg.keymap.trim().is_empty() {
        return Err(PlanError::BadKeymap);
    }
    if cfg.hostname.trim().is_empty() {
        return Err(PlanError::BadHostname);
    }
    if !valid_username(&cfg.username) {
        return Err(PlanError::BadUsername);
    }
    // Live-modus stelt geen schijf-eisen.
    if !cfg.live {
        let need = minimum_disk_bytes();
        if cfg.disk.total_bytes < need {
            return Err(PlanError::DiskTooSmall { need_bytes: need, have_bytes: cfg.disk.total_bytes });
        }
    }
    Ok(())
}

/// Bereken de GPT-partitielayout: ESP · EuroOS-A · EuroOS-B · EuroVar.
/// De twee systeem-slots zijn gelijk; EuroVar krijgt de rest.
fn partition_layout(disk: Disk) -> Vec<PartitionPlan> {
    let align = MIB / SECTOR; // 1 MiB-uitlijning
    let total_sectors = disk.total_bytes / SECTOR;
    // GPT reserveert sector 0 (MBR) + 1 (header) + 32 (entries) aan elk uiteinde.
    let first = align; // begin bij 1 MiB
    let last_usable = total_sectors - 34;

    let esp = ESP_BYTES / SECTOR;
    // Verdeel de rest: twee gelijke slots + var (slots krijgen elk 3/8, var 2/8).
    let remaining = last_usable - first - esp;
    let slot = ((remaining * 3 / 8) / align) * align;
    let mut p = Vec::new();
    let mut cur = first;
    p.push(PartitionPlan { label: "EuroESP", start_lba: cur, sectors: esp });
    cur += esp;
    p.push(PartitionPlan { label: "EuroOS-A", start_lba: cur, sectors: slot });
    cur += slot;
    p.push(PartitionPlan { label: "EuroOS-B", start_lba: cur, sectors: slot });
    cur += slot;
    let var = ((last_usable - cur) / align) * align;
    p.push(PartitionPlan { label: "EuroVar", start_lba: cur, sectors: var });
    p
}

/// Bouw het volledige, geordende installatieplan. Faalt als de config ongeldig is.
pub fn plan(cfg: &Config) -> Result<Vec<Step>, PlanError> {
    validate(cfg)?;
    let mut steps = Vec::new();

    if cfg.live {
        // Live-boot: geen schijf-schrijfacties — enkel runtime-configuratie in RAM.
        steps.push(Step::ConfigureLocale(cfg.locale.clone()));
        steps.push(Step::ConfigureKeymap(cfg.keymap.clone()));
        steps.push(Step::SetHostname(cfg.hostname.clone()));
        steps.push(Step::ProvisionEuroCa);
        return Ok(steps);
    }

    // Volledige installatie naar schijf.
    steps.push(Step::Partition(partition_layout(cfg.disk)));
    steps.push(Step::FormatEsp);
    if cfg.fde {
        // FDE moet vóór het FS-format: het FS leeft op de versleutelde laag.
        steps.push(Step::EnrollFde);
    }
    steps.push(Step::FormatSystem);
    steps.push(Step::FormatVar);
    steps.push(Step::WriteKernelSlots);
    steps.push(Step::InstallLoader);
    steps.push(Step::ConfigureLocale(cfg.locale.clone()));
    steps.push(Step::ConfigureKeymap(cfg.keymap.clone()));
    steps.push(Step::SetHostname(cfg.hostname.clone()));
    steps.push(Step::CreateUser(cfg.username.clone()));
    steps.push(Step::ProvisionEuroCa);
    steps.push(Step::FinalizeBoot);
    Ok(steps)
}

/// Een mensvriendelijke één-regel-omschrijving van een stap (voor de UI/log).
pub fn describe(step: &Step) -> String {
    match step {
        Step::Partition(p) => alloc::format!("GPT schrijven ({} partities)", p.len()),
        Step::FormatEsp => "ESP formatteren (FAT32)".to_string(),
        Step::FormatSystem => "EuroFS aanleggen op slot A/B".to_string(),
        Step::FormatVar => "EuroVar aanleggen".to_string(),
        Step::WriteKernelSlots => "kernel naar slot A + B schrijven".to_string(),
        Step::InstallLoader => "twee-traps-loader op ESP".to_string(),
        Step::EnrollFde => "full-disk-encryptie inschakelen (TPM-verzegeld)".to_string(),
        Step::ConfigureLocale(l) => alloc::format!("locale instellen: {l}"),
        Step::ConfigureKeymap(k) => alloc::format!("toetsenbord: {k}"),
        Step::SetHostname(h) => alloc::format!("hostnaam: {h}"),
        Step::CreateUser(u) => alloc::format!("gebruiker aanmaken: {u}"),
        Step::ProvisionEuroCa => "EuroCA provisioneren".to_string(),
        Step::FinalizeBoot => "A/B-bootconfig + slot A activeren".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(total_gib: u64, live: bool, fde: bool) -> Config {
        Config {
            disk: Disk { total_bytes: total_gib * 1024 * MIB },
            locale: "nl-BE".to_string(),
            keymap: "be-azerty".to_string(),
            hostname: "euro-pc".to_string(),
            username: "anke".to_string(),
            fde,
            live,
        }
    }

    #[test]
    fn validates_good_config() {
        assert_eq!(validate(&cfg(8, false, true)), Ok(()));
    }

    #[test]
    fn rejects_small_disk() {
        let c = cfg(1, false, false); // 1 GiB < minimum (~1.5 GiB)
        assert!(matches!(validate(&c), Err(PlanError::DiskTooSmall { .. })));
        // Maar in live-modus maakt schijfgrootte niet uit.
        let mut live = c.clone();
        live.live = true;
        assert_eq!(validate(&live), Ok(()));
    }

    #[test]
    fn rejects_bad_username_and_locale() {
        let mut c = cfg(8, false, false);
        c.username = "Anke!".to_string();
        assert_eq!(validate(&c), Err(PlanError::BadUsername));
        let mut c2 = cfg(8, false, false);
        c2.locale = "xx-YY".to_string();
        assert_eq!(validate(&c2), Err(PlanError::BadLocale));
    }

    #[test]
    fn partition_layout_fits_disk() {
        let disk = Disk { total_bytes: 8 * 1024 * MIB };
        let parts = partition_layout(disk);
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].label, "EuroESP");
        assert_eq!(parts[3].label, "EuroVar");
        // Partities overlappen niet en blijven binnen de schijf.
        for w in parts.windows(2) {
            assert!(w[0].start_lba + w[0].sectors <= w[1].start_lba);
        }
        let last = &parts[3];
        assert!((last.start_lba + last.sectors) * SECTOR <= disk.total_bytes);
        // De twee slots zijn even groot.
        assert_eq!(parts[1].sectors, parts[2].sectors);
    }

    #[test]
    fn plan_disk_order() {
        let steps = plan(&cfg(8, false, true)).unwrap();
        // FDE komt vóór het FS-format.
        let fde = steps.iter().position(|s| *s == Step::EnrollFde).unwrap();
        let fmt = steps.iter().position(|s| *s == Step::FormatSystem).unwrap();
        assert!(fde < fmt);
        // Partitioneren is de eerste stap, FinalizeBoot de laatste.
        assert!(matches!(steps.first(), Some(Step::Partition(_))));
        assert_eq!(steps.last(), Some(&Step::FinalizeBoot));
        assert!(steps.iter().any(|s| *s == Step::CreateUser("anke".to_string())));
    }

    #[test]
    fn plan_live_skips_disk() {
        let steps = plan(&cfg(8, true, false)).unwrap();
        assert!(!steps.iter().any(|s| matches!(s, Step::Partition(_))));
        assert!(!steps.iter().any(|s| *s == Step::FormatSystem));
        assert!(steps.iter().any(|s| matches!(s, Step::ConfigureLocale(_))));
    }

    #[test]
    fn no_fde_skips_enrol() {
        let steps = plan(&cfg(8, false, false)).unwrap();
        assert!(!steps.iter().any(|s| *s == Step::EnrollFde));
    }
}
