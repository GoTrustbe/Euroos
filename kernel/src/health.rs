//! Kernel-zijde van **EuroHealth** (plan Z): combineer NVMe-SMART, FS-integriteit
//! (scrub) en geheugen-status tot een gezondheidsrapport + score. Voedt EuroObserve
//! (W) en zou bij een dalende score een EuroDisplay-waarschuwing geven.

use alloc::string::String;
use alloc::vec::Vec;

use eurohealth::{HealthReport, SmartHealth, SmartStatus};

/// Bouw het gezondheidsrapport: SMART (als er NVMe is) + FS-scrub + geheugen.
pub fn report(fs_errors: usize, fs_unrecoverable: usize, free_frames: u64, total_frames: u64) -> HealthReport {
    let disk = if crate::nvme::present() {
        crate::nvme::smart_log().and_then(|log| SmartHealth::parse(&log))
    } else {
        None
    };
    HealthReport { disk, fs_errors, fs_unrecoverable, free_frames, total_frames }
}

fn status_str(s: SmartStatus) -> &'static str {
    match s {
        SmartStatus::Passed => "GEZOND",
        SmartStatus::Warning => "WAARSCHUWING",
        SmartStatus::Failed => "KRITIEK",
    }
}

/// Boot-zelftest: rapporteer de systeemgezondheid.
pub fn selftest(fs_errors: usize, fs_unrecoverable: usize, free_frames: u64, total_frames: u64) {
    let r = report(fs_errors, fs_unrecoverable, free_frames, total_frames);
    let disk_part = match &r.disk {
        Some(d) => alloc::format!(
            "SMART {} (score {}, {}°C, spare {}%, slijtage {}%, media-fouten {})",
            status_str(d.status()), d.score(), d.temperature_c, d.available_spare, d.percentage_used, d.media_errors
        ),
        None => String::from("geen NVMe-schijf (SMART n/b)"),
    };
    crate::serial_println!(
        "[z] EuroHealth: {disk_part}; FS-fouten={fs_errors}/onherstelbaar={fs_unrecoverable}; vrij {}/{} frames → totaalscore {}/100 = {} ✓",
        free_frames, total_frames, r.overall_score(), status_str(r.summary())
    );
}

/// `eurohealth`-shellcommando.
pub fn shell(fs_errors: usize, fs_unrecoverable: usize, free_frames: u64, total_frames: u64) -> Vec<String> {
    let r = report(fs_errors, fs_unrecoverable, free_frames, total_frames);
    let mut v = alloc::vec![alloc::format!(
        "EuroHealth — totaalscore {}/100 ({})",
        r.overall_score(), status_str(r.summary())
    )];
    match &r.disk {
        Some(d) => {
            v.push(alloc::format!("  schijf (SMART): {} — score {}/100", status_str(d.status()), d.score()));
            v.push(alloc::format!("    temperatuur {}°C, spare {}% (drempel {}%), slijtage {}%", d.temperature_c, d.available_spare, d.spare_threshold, d.percentage_used));
            v.push(alloc::format!("    power-on-uren {}, media-fouten {}, onveilige-shutdowns {}", d.power_on_hours, d.media_errors, d.unsafe_shutdowns));
        }
        None => v.push(String::from("  schijf: geen NVMe (SMART niet beschikbaar)")),
    }
    v.push(alloc::format!("  filesysteem: {} scrub-fouten, {} onherstelbaar", fs_errors, fs_unrecoverable));
    v.push(alloc::format!("  geheugen: {}/{} frames vrij", free_frames, total_frames));
    v
}
