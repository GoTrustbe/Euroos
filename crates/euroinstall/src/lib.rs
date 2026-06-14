//! EuroInstall — the **planner** for the guided installation + live image (plan Q1).
//!
//! The installer is deliberately split: the *decision logic* (partition layout,
//! validation, the ordered step plan) is pure, host-tested `no_std` code; the
//! *execution* (real sector I/O via `gpt`/`eurofs`, FDE enrol via `eurofde`,
//! user via `auth`) is wired in by the kernel/the userspace installer process.
//! This way the fragile part — the disk layout — is fully testable without a disk.
//!
//! Two modes: a real **installation** to disk (A/B slots + EuroVar + ESP) and
//! a **live** boot that runs entirely in RAM and leaves the disk untouched.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

const SECTOR: u64 = 512;
const MIB: u64 = 1024 * 1024;
/// ESP (EFI System Partition) — FAT32 with the loader + A/B kernels.
const ESP_BYTES: u64 = 256 * MIB;
/// Minimum size per system slot.
const SLOT_MIN_BYTES: u64 = 512 * MIB;
/// Minimum EuroVar (writable data).
const VAR_MIN_BYTES: u64 = 256 * MIB;

/// The geometry of the target disk.
#[derive(Clone, Copy, Debug)]
pub struct Disk {
    pub total_bytes: u64,
}

/// The choices the user (or the unattended config) makes.
#[derive(Clone, Debug)]
pub struct Config {
    pub disk: Disk,
    /// BCP-47 language tag, e.g. `"nl-BE"` — must be a valid EU language.
    pub locale: String,
    /// Keyboard layout, e.g. `"be-azerty"`.
    pub keymap: String,
    pub hostname: String,
    pub username: String,
    /// Enable full-disk encryption (EuroFDE, key via TPM).
    pub fde: bool,
    /// Live mode: write nothing to disk, everything in RAM.
    pub live: bool,
}

/// A planned GPT partition (in 512-byte sectors).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionPlan {
    pub label: &'static str,
    pub start_lba: u64,
    pub sectors: u64,
}

/// A single step in the installation plan, in execution order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Write the GPT with these partitions.
    Partition(Vec<PartitionPlan>),
    /// Format the ESP (FAT32).
    FormatEsp,
    /// Create EuroFS on slot A (and copy to B).
    FormatSystem,
    /// Create EuroFS on the EuroVar partition.
    FormatVar,
    /// Write the kernel image to slot A and B.
    WriteKernelSlots,
    /// Install the two-stage loader on the ESP.
    InstallLoader,
    /// Enable FDE (ChaCha20, key sealed to the TPM).
    EnrollFde,
    /// Set the locale (uses EuroLocale).
    ConfigureLocale(String),
    /// Set the keyboard layout.
    ConfigureKeymap(String),
    /// Set the hostname.
    SetHostname(String),
    /// Create the first user (with sudo).
    CreateUser(String),
    /// Provision the EuroCA (local certificate authority).
    ProvisionEuroCa,
    /// Write the A/B boot config + activate slot A.
    FinalizeBoot,
}

/// Why a config is rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// The disk is too small for an A/B installation.
    DiskTooSmall { need_bytes: u64, have_bytes: u64 },
    /// Invalid username (empty, too long, or not [a-z0-9_-]).
    BadUsername,
    /// Empty hostname.
    BadHostname,
    /// Unknown language tag (not an EU language).
    BadLocale,
    /// Empty keymap.
    BadKeymap,
}

/// The minimum number of bytes required for an installation to disk.
pub fn minimum_disk_bytes() -> u64 {
    ESP_BYTES + 2 * SLOT_MIN_BYTES + VAR_MIN_BYTES
}

fn valid_username(u: &str) -> bool {
    !u.is_empty()
        && u.len() <= 32
        && u.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        && u.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
}

/// Validate a config without building a plan.
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
    // Live mode imposes no disk requirements.
    if !cfg.live {
        let need = minimum_disk_bytes();
        if cfg.disk.total_bytes < need {
            return Err(PlanError::DiskTooSmall { need_bytes: need, have_bytes: cfg.disk.total_bytes });
        }
    }
    Ok(())
}

/// Compute the GPT partition layout: ESP · EuroOS-A · EuroOS-B · EuroVar.
/// The two system slots are equal; EuroVar gets the rest.
fn partition_layout(disk: Disk) -> Vec<PartitionPlan> {
    let align = MIB / SECTOR; // 1 MiB alignment
    let total_sectors = disk.total_bytes / SECTOR;
    // GPT reserves sector 0 (MBR) + 1 (header) + 32 (entries) at each end.
    let first = align; // start at 1 MiB
    let last_usable = total_sectors - 34;

    let esp = ESP_BYTES / SECTOR;
    // Divide the rest: two equal slots + var (slots get 3/8 each, var 2/8).
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

/// Build the full, ordered installation plan. Fails if the config is invalid.
pub fn plan(cfg: &Config) -> Result<Vec<Step>, PlanError> {
    validate(cfg)?;
    let mut steps = Vec::new();

    if cfg.live {
        // Live boot: no disk writes — only runtime configuration in RAM.
        steps.push(Step::ConfigureLocale(cfg.locale.clone()));
        steps.push(Step::ConfigureKeymap(cfg.keymap.clone()));
        steps.push(Step::SetHostname(cfg.hostname.clone()));
        steps.push(Step::ProvisionEuroCa);
        return Ok(steps);
    }

    // Full installation to disk.
    steps.push(Step::Partition(partition_layout(cfg.disk)));
    steps.push(Step::FormatEsp);
    if cfg.fde {
        // FDE must come before the FS format: the FS lives on the encrypted layer.
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

/// A human-friendly one-line description of a step (for the UI/log).
pub fn describe(step: &Step) -> String {
    match step {
        Step::Partition(p) => alloc::format!("write GPT ({} partitions)", p.len()),
        Step::FormatEsp => "format ESP (FAT32)".to_string(),
        Step::FormatSystem => "create EuroFS on slot A/B".to_string(),
        Step::FormatVar => "create EuroVar".to_string(),
        Step::WriteKernelSlots => "write kernel to slot A + B".to_string(),
        Step::InstallLoader => "two-stage loader on ESP".to_string(),
        Step::EnrollFde => "enable full-disk encryption (TPM-sealed)".to_string(),
        Step::ConfigureLocale(l) => alloc::format!("set locale: {l}"),
        Step::ConfigureKeymap(k) => alloc::format!("keyboard: {k}"),
        Step::SetHostname(h) => alloc::format!("hostname: {h}"),
        Step::CreateUser(u) => alloc::format!("create user: {u}"),
        Step::ProvisionEuroCa => "provision EuroCA".to_string(),
        Step::FinalizeBoot => "A/B boot config + activate slot A".to_string(),
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
        // But in live mode the disk size does not matter.
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
        // Partitions do not overlap and stay within the disk.
        for w in parts.windows(2) {
            assert!(w[0].start_lba + w[0].sectors <= w[1].start_lba);
        }
        let last = &parts[3];
        assert!((last.start_lba + last.sectors) * SECTOR <= disk.total_bytes);
        // The two slots are equal in size.
        assert_eq!(parts[1].sectors, parts[2].sectors);
    }

    #[test]
    fn plan_disk_order() {
        let steps = plan(&cfg(8, false, true)).unwrap();
        // FDE comes before the FS format.
        let fde = steps.iter().position(|s| *s == Step::EnrollFde).unwrap();
        let fmt = steps.iter().position(|s| *s == Step::FormatSystem).unwrap();
        assert!(fde < fmt);
        // Partitioning is the first step, FinalizeBoot the last.
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
