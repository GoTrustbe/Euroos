//! EuroHealth — **systeem-gezondheid** (plan Z).
//!
//! NVMe-schijven houden een **SMART/Health Information**-logpagina bij (NVMe-log id
//! 0x02): temperatuur, beschikbare reserve-blokken, slijtage, media-fouten,
//! power-on-uren. EuroHealth parset die log, combineert 'm met FS-integriteit
//! (scrub) en geheugen-status tot een **gezondheidsscore + voorspellende
//! waarschuwing**. Pure `no_std`-parsing/​scoring → host-getest, los van de driver.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// SMART-status-oordeel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartStatus {
    Passed,
    Warning,
    Failed,
}

/// De relevante velden uit de NVMe-SMART-logpagina (0x02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmartHealth {
    pub critical_warning: u8, // bitmap: bit0=spare laag, bit1=temp, bit3=read-only, ...
    pub temperature_c: i16,   // composite-temperatuur (uit Kelvin)
    pub available_spare: u8,  // % reserve-blokken over
    pub spare_threshold: u8,  // % drempel
    pub percentage_used: u8,  // geschatte slijtage (0..100+, kan >100)
    pub power_on_hours: u64,
    pub media_errors: u64,
    pub unsafe_shutdowns: u64,
}

/// Lees een 128-bit little-endian veld (de meeste SMART-tellers) als u64 (lage helft).
fn r128_lo(b: &[u8], o: usize) -> u64 {
    let mut v = 0u64;
    for i in 0..8 {
        v |= (b[o + i] as u64) << (8 * i);
    }
    v
}

impl SmartHealth {
    /// Parse de SMART/Health-logpagina (≥ 192 bytes).
    pub fn parse(log: &[u8]) -> Option<SmartHealth> {
        if log.len() < 192 {
            return None;
        }
        let temp_k = u16::from_le_bytes([log[1], log[2]]) as i32;
        Some(SmartHealth {
            critical_warning: log[0],
            temperature_c: (temp_k - 273) as i16,
            available_spare: log[3],
            spare_threshold: log[4],
            percentage_used: log[5],
            power_on_hours: r128_lo(log, 128),
            media_errors: r128_lo(log, 144),
            unsafe_shutdowns: r128_lo(log, 112),
        })
    }

    /// Het status-oordeel: Failed bij een critical-warning of spare-onder-drempel;
    /// Warning bij hoge slijtage / temperatuur / media-fouten; anders Passed.
    pub fn status(&self) -> SmartStatus {
        if self.critical_warning != 0 || (self.spare_threshold > 0 && self.available_spare < self.spare_threshold) {
            return SmartStatus::Failed;
        }
        if self.percentage_used >= 90 || self.temperature_c >= 70 || self.media_errors > 0 {
            return SmartStatus::Warning;
        }
        SmartStatus::Passed
    }

    /// Gezondheidsscore 0..100 (100 = perfect).
    pub fn score(&self) -> u8 {
        let mut s: i32 = 100;
        if self.critical_warning != 0 {
            s -= 50;
        }
        s -= self.percentage_used.min(100) as i32 / 2; // slijtage telt voor max 50
        if self.media_errors > 0 {
            s -= 15;
        }
        if self.temperature_c >= 70 {
            s -= 10;
        }
        if self.spare_threshold > 0 && self.available_spare < self.spare_threshold {
            s -= 30;
        }
        s.clamp(0, 100) as u8
    }
}

/// Het volledige gezondheidsrapport (schijf + FS + geheugen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthReport {
    pub disk: Option<SmartHealth>,
    pub fs_errors: usize,   // scrub-fouten
    pub fs_unrecoverable: usize,
    pub free_frames: u64,
    pub total_frames: u64,
}

impl HealthReport {
    /// Een gecombineerde score (0..100) over schijf + FS + geheugendruk.
    pub fn overall_score(&self) -> u8 {
        let mut s: i32 = 100;
        if let Some(d) = &self.disk {
            s = s.min(d.score() as i32);
        }
        if self.fs_errors > 0 {
            s -= 20;
        }
        if self.fs_unrecoverable > 0 {
            s -= 40;
        }
        // Geheugendruk: < 5% vrij → aftrek.
        if self.total_frames > 0 && self.free_frames * 20 < self.total_frames {
            s -= 15;
        }
        s.clamp(0, 100) as u8
    }

    pub fn summary(&self) -> SmartStatus {
        let sc = self.overall_score();
        if sc >= 90 {
            SmartStatus::Passed
        } else if sc >= 60 {
            SmartStatus::Warning
        } else {
            SmartStatus::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy_log() -> [u8; 512] {
        let mut l = [0u8; 512];
        l[0] = 0; // geen warning
        l[1..3].copy_from_slice(&((273 + 35) as u16).to_le_bytes()); // 35 °C
        l[3] = 100; // 100% spare
        l[4] = 10; // drempel 10%
        l[5] = 3; // 3% slijtage
        l[128..136].copy_from_slice(&12000u64.to_le_bytes()); // power-on-uren
        l
    }

    #[test]
    fn parse_healthy() {
        let h = SmartHealth::parse(&healthy_log()).unwrap();
        assert_eq!(h.temperature_c, 35);
        assert_eq!(h.available_spare, 100);
        assert_eq!(h.percentage_used, 3);
        assert_eq!(h.power_on_hours, 12000);
        assert_eq!(h.status(), SmartStatus::Passed);
        assert!(h.score() >= 95);
    }

    #[test]
    fn failing_disk() {
        let mut l = healthy_log();
        l[0] = 0x01; // critical warning: spare laag
        l[3] = 5; // 5% spare, onder drempel 10
        let h = SmartHealth::parse(&l).unwrap();
        assert_eq!(h.status(), SmartStatus::Failed);
        assert!(h.score() < 50);
    }

    #[test]
    fn worn_disk_is_warning() {
        let mut l = healthy_log();
        l[5] = 95; // 95% slijtage
        let h = SmartHealth::parse(&l).unwrap();
        assert_eq!(h.status(), SmartStatus::Warning);
    }

    #[test]
    fn overall_report() {
        let h = SmartHealth::parse(&healthy_log()).unwrap();
        let r = HealthReport {
            disk: Some(h),
            fs_errors: 0,
            fs_unrecoverable: 0,
            free_frames: 10000,
            total_frames: 16000,
        };
        assert!(r.overall_score() >= 90);
        assert_eq!(r.summary(), SmartStatus::Passed);
        // Met FS-corruptie zakt 't naar Failed.
        let bad = HealthReport { fs_unrecoverable: 2, ..r };
        assert!(bad.overall_score() < 60);
        assert_eq!(bad.summary(), SmartStatus::Failed);
    }
}
