//! Kernel side of **EuroHealth** (plan Z): combine NVMe SMART, FS integrity
//! (scrub) and memory status into a health report + score. Feeds EuroObserve
//! (W) and would raise a EuroDisplay warning on a falling score.

use alloc::string::String;
use alloc::vec::Vec;

use eurohealth::{HealthReport, SmartHealth, SmartStatus};

/// Build the health report: SMART (if there is NVMe) + FS scrub + memory.
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
        SmartStatus::Passed => "HEALTHY",
        SmartStatus::Warning => "WARNING",
        SmartStatus::Failed => "CRITICAL",
    }
}

/// Boot self-test: report the system health.
pub fn selftest(fs_errors: usize, fs_unrecoverable: usize, free_frames: u64, total_frames: u64) {
    let r = report(fs_errors, fs_unrecoverable, free_frames, total_frames);
    let disk_part = match &r.disk {
        Some(d) => alloc::format!(
            "SMART {} (score {}, {}°C, spare {}%, wear {}%, media errors {})",
            status_str(d.status()), d.score(), d.temperature_c, d.available_spare, d.percentage_used, d.media_errors
        ),
        None => String::from("no NVMe disk (SMART n/a)"),
    };
    crate::serial_println!(
        "[z] EuroHealth: {disk_part}; FS-errors={fs_errors}/unrecoverable={fs_unrecoverable}; free {}/{} frames → total score {}/100 = {} ✓",
        free_frames, total_frames, r.overall_score(), status_str(r.summary())
    );
}

/// `eurohealth` shell command.
pub fn shell(fs_errors: usize, fs_unrecoverable: usize, free_frames: u64, total_frames: u64) -> Vec<String> {
    let r = report(fs_errors, fs_unrecoverable, free_frames, total_frames);
    let mut v = alloc::vec![alloc::format!(
        "EuroHealth — total score {}/100 ({})",
        r.overall_score(), status_str(r.summary())
    )];
    match &r.disk {
        Some(d) => {
            v.push(alloc::format!("  disk (SMART): {} — score {}/100", status_str(d.status()), d.score()));
            v.push(alloc::format!("    temperature {}°C, spare {}% (threshold {}%), wear {}%", d.temperature_c, d.available_spare, d.spare_threshold, d.percentage_used));
            v.push(alloc::format!("    power-on hours {}, media errors {}, unsafe shutdowns {}", d.power_on_hours, d.media_errors, d.unsafe_shutdowns));
        }
        None => v.push(String::from("  disk: no NVMe (SMART not available)")),
    }
    v.push(alloc::format!("  filesystem: {} scrub errors, {} unrecoverable", fs_errors, fs_unrecoverable));
    v.push(alloc::format!("  memory: {}/{} frames free", free_frames, total_frames));
    v
}
