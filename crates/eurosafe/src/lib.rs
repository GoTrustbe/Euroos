//! EuroSafe — the privacy/capability dashboard of EuroOS (Sprint AC-1).
//!
//! The **visible face of EuroGuard**: no mainstream OS shows a
//! realtime overview of which app holds which kernel capabilities. This crate
//! is the pure model + the risk scoring + the summary that the kernel populates
//! from the live EuroGuard/EuroPol state and the compositor renders.
//!
//! Pure `no_std` logic, host-tested. The kernel maps its own
//! capability representation onto [`Capability`].

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A kernel capability that an app can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Access to the key vault (EuroVault) — the most sensitive.
    Vault,
    /// Run other programs.
    Exec,
    /// Camera.
    Camera,
    /// Microphone.
    Microphone,
    /// Location.
    Location,
    /// Write to files.
    FileWrite,
    /// Network access.
    Net,
    /// Read files.
    FileRead,
    /// Play audio.
    Audio,
    /// Draw on the screen.
    Display,
    /// Clipboard.
    Clipboard,
    /// Show notifications.
    Notifications,
}

impl Capability {
    /// Risk weight (higher = more sensitive).
    pub fn weight(self) -> u32 {
        match self {
            Capability::Vault => 10,
            Capability::Exec => 9,
            Capability::Camera => 7,
            Capability::Microphone => 7,
            Capability::Location => 5,
            Capability::FileWrite => 6,
            Capability::Net => 5,
            Capability::FileRead => 3,
            Capability::Audio => 2,
            Capability::Clipboard => 2,
            Capability::Display => 1,
            Capability::Notifications => 1,
        }
    }

    /// Human-friendly name.
    pub fn label(self) -> &'static str {
        match self {
            Capability::Vault => "Key vault",
            Capability::Exec => "Run programs",
            Capability::Camera => "Camera",
            Capability::Microphone => "Microphone",
            Capability::Location => "Location",
            Capability::FileWrite => "Write files",
            Capability::Net => "Network",
            Capability::FileRead => "Read files",
            Capability::Audio => "Audio",
            Capability::Display => "Display",
            Capability::Clipboard => "Clipboard",
            Capability::Notifications => "Notifications",
        }
    }
}

/// Risk classification of an app or the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::Low => "Low",
            Risk::Medium => "Medium",
            Risk::High => "High",
        }
    }
}

/// The capabilities one app holds, plus whether it runs sandboxed.
#[derive(Debug, Clone)]
pub struct AppPermissions {
    pub app: String,
    pub caps: Vec<Capability>,
    /// Does the app run in an EuroGuard sandbox (lowers the effective risk)?
    pub sandboxed: bool,
    /// Is the app signed/verified (EuroGuard manifest)?
    pub verified: bool,
}

impl AppPermissions {
    pub fn new(app: &str) -> Self {
        AppPermissions { app: app.to_string(), caps: Vec::new(), sandboxed: true, verified: true }
    }
    pub fn with(mut self, cap: Capability) -> Self {
        if !self.caps.contains(&cap) {
            self.caps.push(cap);
        }
        self
    }
    pub fn sandboxed(mut self, yes: bool) -> Self {
        self.sandboxed = yes;
        self
    }
    pub fn verified(mut self, yes: bool) -> Self {
        self.verified = yes;
        self
    }

    /// Raw risk score = sum of capability weights.
    pub fn raw_score(&self) -> u32 {
        self.caps.iter().map(|c| c.weight()).sum()
    }

    /// Effective score: sandbox dampens (×0.6, rounded up); a
    /// non-verified app gets a surcharge (×1.5).
    pub fn effective_score(&self) -> u32 {
        let mut s = self.raw_score();
        if self.sandboxed {
            s = (s * 6).div_ceil(10);
        }
        if !self.verified {
            s = (s * 15).div_ceil(10);
        }
        s
    }

    /// Risk class based on the effective score.
    pub fn risk(&self) -> Risk {
        classify(self.effective_score())
    }

    /// A dangerous combination (vault + network + exec) → exfiltration risk.
    pub fn is_dangerous_combo(&self) -> bool {
        let has = |c| self.caps.contains(&c);
        has(Capability::Vault) && has(Capability::Net) && has(Capability::Exec)
    }
}

fn classify(score: u32) -> Risk {
    if score >= 15 {
        Risk::High
    } else if score >= 7 {
        Risk::Medium
    } else {
        Risk::Low
    }
}

/// One event from the audit log.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub app: String,
    pub cap: Capability,
    pub allowed: bool,
}

/// The full dashboard: apps + recent audit events.
#[derive(Debug, Clone, Default)]
pub struct Dashboard {
    pub apps: Vec<AppPermissions>,
    pub events: Vec<AuditEvent>,
}

/// A recommendation for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    pub app: String,
    pub message: String,
    pub severity: Risk,
}

impl Dashboard {
    pub fn new() -> Self {
        Dashboard::default()
    }
    pub fn add_app(&mut self, app: AppPermissions) {
        self.apps.push(app);
    }
    pub fn add_event(&mut self, ev: AuditEvent) {
        self.events.push(ev);
    }

    /// Apps with a high risk.
    pub fn high_risk_apps(&self) -> Vec<&AppPermissions> {
        self.apps.iter().filter(|a| a.risk() == Risk::High).collect()
    }

    /// All capabilities in use with the number of apps that hold them,
    /// sorted from sensitive to less sensitive.
    pub fn caps_in_use(&self) -> Vec<(Capability, usize)> {
        let order = [
            Capability::Vault,
            Capability::Exec,
            Capability::Camera,
            Capability::Microphone,
            Capability::Location,
            Capability::FileWrite,
            Capability::Net,
            Capability::FileRead,
            Capability::Audio,
            Capability::Display,
            Capability::Clipboard,
            Capability::Notifications,
        ];
        let mut out = Vec::new();
        for cap in order {
            let n = self.apps.iter().filter(|a| a.caps.contains(&cap)).count();
            if n > 0 {
                out.push((cap, n));
            }
        }
        out
    }

    /// Number of denied audit events (EuroGuard blocked something).
    pub fn denied_count(&self) -> usize {
        self.events.iter().filter(|e| !e.allowed).count()
    }

    /// System-wide risk class = the highest app class.
    pub fn system_risk(&self) -> Risk {
        if self.apps.iter().any(|a| a.risk() == Risk::High) {
            Risk::High
        } else if self.apps.iter().any(|a| a.risk() == Risk::Medium) {
            Risk::Medium
        } else {
            Risk::Low
        }
    }

    /// Concrete recommendations (dangerous combos, unverified apps with vault...).
    pub fn recommendations(&self) -> Vec<Recommendation> {
        let mut recs = Vec::new();
        for a in &self.apps {
            if a.is_dangerous_combo() {
                recs.push(Recommendation {
                    app: a.app.clone(),
                    message: "Holds vault + network + exec — exfiltration risk. Restrict one of these.".to_string(),
                    severity: Risk::High,
                });
            }
            if !a.verified && a.caps.contains(&Capability::Vault) {
                recs.push(Recommendation {
                    app: a.app.clone(),
                    message: "Unverified app with key vault access. Revoke the access.".to_string(),
                    severity: Risk::High,
                });
            }
            if !a.sandboxed {
                recs.push(Recommendation {
                    app: a.app.clone(),
                    message: "Runs outside the sandbox. Put the app in an EuroGuard sandbox.".to_string(),
                    severity: Risk::Medium,
                });
            }
        }
        recs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_classification() {
        // Light app: display + audio → low.
        let viewer = AppPermissions::new("EuroMedia")
            .with(Capability::Display)
            .with(Capability::Audio)
            .with(Capability::FileRead);
        assert_eq!(viewer.risk(), Risk::Low);

        // Heavy, unsandboxed, unverified app → high.
        let bad = AppPermissions::new("rare.bin")
            .with(Capability::Vault)
            .with(Capability::Exec)
            .with(Capability::Net)
            .sandboxed(false)
            .verified(false);
        assert_eq!(bad.risk(), Risk::High);
    }

    #[test]
    fn sandbox_lowers_score() {
        let caps = || {
            AppPermissions::new("x")
                .with(Capability::Net)
                .with(Capability::FileWrite)
                .with(Capability::FileRead)
        };
        let sandboxed = caps().sandboxed(true).effective_score();
        let bare = caps().sandboxed(false).effective_score();
        assert!(sandboxed < bare);
    }

    #[test]
    fn dangerous_combo_detected() {
        let app = AppPermissions::new("agent")
            .with(Capability::Vault)
            .with(Capability::Net)
            .with(Capability::Exec);
        assert!(app.is_dangerous_combo());
        let safe = AppPermissions::new("editor")
            .with(Capability::FileRead)
            .with(Capability::FileWrite);
        assert!(!safe.is_dangerous_combo());
    }

    #[test]
    fn dashboard_aggregation() {
        let mut db = Dashboard::new();
        db.add_app(
            AppPermissions::new("EuroMail")
                .with(Capability::Net)
                .with(Capability::Vault)
                .with(Capability::FileRead),
        );
        db.add_app(AppPermissions::new("EuroReken").with(Capability::Display));
        db.add_event(AuditEvent { app: "EuroMail".into(), cap: Capability::Net, allowed: true });
        db.add_event(AuditEvent { app: "rare.bin".into(), cap: Capability::Vault, allowed: false });

        let caps = db.caps_in_use();
        // Vault comes before Net before Display (sorted by sensitivity).
        assert_eq!(caps[0].0, Capability::Vault);
        assert!(caps.iter().any(|(c, n)| *c == Capability::Display && *n == 1));
        assert_eq!(db.denied_count(), 1);
    }

    #[test]
    fn recommendations_flag_problems() {
        let mut db = Dashboard::new();
        db.add_app(
            AppPermissions::new("sketchy")
                .with(Capability::Vault)
                .with(Capability::Net)
                .with(Capability::Exec)
                .verified(false)
                .sandboxed(false),
        );
        let recs = db.recommendations();
        // combo + unverified-vault + outside-sandbox = 3 recommendations.
        assert_eq!(recs.len(), 3);
        assert!(recs.iter().any(|r| r.severity == Risk::High));
        assert_eq!(db.system_risk(), Risk::High);
    }

    #[test]
    fn capability_labels_and_weights() {
        assert_eq!(Capability::Vault.label(), "Key vault");
        assert!(Capability::Vault.weight() > Capability::Display.weight());
        assert_eq!(Risk::High.label(), "High");
    }
}
