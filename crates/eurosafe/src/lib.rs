//! EuroSafe — het privacy-/capability-dashboard van EuroOS (Sprint AC-1).
//!
//! Het **zichtbare gezicht van EuroGuard**: geen mainstream OS toont een
//! realtime overzicht van welke app welke kernel-capabilities bezit. Deze crate
//! is het pure model + de risico-scoring + de samenvatting die de kernel uit de
//! live EuroGuard/EuroPol-staat vult en de compositor rendert.
//!
//! Pure `no_std`-logica, host-getest. De kernel mapt zijn eigen
//! capability-representatie naar [`Capability`].

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Een kernel-capability die een app kan bezitten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Toegang tot de sleutelkluis (EuroVault) — het gevoeligst.
    Vault,
    /// Andere programma's uitvoeren.
    Exec,
    /// Camera.
    Camera,
    /// Microfoon.
    Microphone,
    /// Locatie.
    Location,
    /// Naar bestanden schrijven.
    FileWrite,
    /// Netwerktoegang.
    Net,
    /// Bestanden lezen.
    FileRead,
    /// Geluid afspelen.
    Audio,
    /// Op het scherm tekenen.
    Display,
    /// Klembord.
    Clipboard,
    /// Meldingen tonen.
    Notifications,
}

impl Capability {
    /// Risicogewicht (hoger = gevoeliger).
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

    /// Mensvriendelijke naam (NL).
    pub fn label(self) -> &'static str {
        match self {
            Capability::Vault => "Sleutelkluis",
            Capability::Exec => "Programma's uitvoeren",
            Capability::Camera => "Camera",
            Capability::Microphone => "Microfoon",
            Capability::Location => "Locatie",
            Capability::FileWrite => "Bestanden schrijven",
            Capability::Net => "Netwerk",
            Capability::FileRead => "Bestanden lezen",
            Capability::Audio => "Geluid",
            Capability::Display => "Scherm",
            Capability::Clipboard => "Klembord",
            Capability::Notifications => "Meldingen",
        }
    }
}

/// Risicoclassificatie van een app of het systeem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::Low => "Laag",
            Risk::Medium => "Gemiddeld",
            Risk::High => "Hoog",
        }
    }
}

/// De capabilities die één app bezit, plus of hij gesandboxed draait.
#[derive(Debug, Clone)]
pub struct AppPermissions {
    pub app: String,
    pub caps: Vec<Capability>,
    /// Draait de app in een EuroGuard-sandbox (verlaagt het effectieve risico)?
    pub sandboxed: bool,
    /// Is de app gesigneerd/geverifieerd (EuroGuard-manifest)?
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

    /// Ruwe risicoscore = som van capability-gewichten.
    pub fn raw_score(&self) -> u32 {
        self.caps.iter().map(|c| c.weight()).sum()
    }

    /// Effectieve score: sandbox dempt (×0,6, naar boven afgerond); een
    /// niet-geverifieerde app krijgt een opslag (×1,5).
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

    /// Risicoklasse op basis van de effectieve score.
    pub fn risk(&self) -> Risk {
        classify(self.effective_score())
    }

    /// Een gevaarlijke combinatie (kluis + netwerk + uitvoeren) → exfiltratie-risico.
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

/// Eén gebeurtenis uit het audit-log.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub app: String,
    pub cap: Capability,
    pub allowed: bool,
}

/// Het volledige dashboard: apps + recente audit-gebeurtenissen.
#[derive(Debug, Clone, Default)]
pub struct Dashboard {
    pub apps: Vec<AppPermissions>,
    pub events: Vec<AuditEvent>,
}

/// Een aanbeveling voor de gebruiker.
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

    /// Apps met een hoog risico.
    pub fn high_risk_apps(&self) -> Vec<&AppPermissions> {
        self.apps.iter().filter(|a| a.risk() == Risk::High).collect()
    }

    /// Alle in gebruik zijnde capabilities met het aantal apps dat ze bezit,
    /// gesorteerd van gevoelig naar minder gevoelig.
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

    /// Aantal geweigerde audit-gebeurtenissen (EuroGuard hield iets tegen).
    pub fn denied_count(&self) -> usize {
        self.events.iter().filter(|e| !e.allowed).count()
    }

    /// Systeembrede risicoklasse = de hoogste app-klasse.
    pub fn system_risk(&self) -> Risk {
        if self.apps.iter().any(|a| a.risk() == Risk::High) {
            Risk::High
        } else if self.apps.iter().any(|a| a.risk() == Risk::Medium) {
            Risk::Medium
        } else {
            Risk::Low
        }
    }

    /// Concrete aanbevelingen (gevaarlijke combo's, onverifieerde apps met kluis...).
    pub fn recommendations(&self) -> Vec<Recommendation> {
        let mut recs = Vec::new();
        for a in &self.apps {
            if a.is_dangerous_combo() {
                recs.push(Recommendation {
                    app: a.app.clone(),
                    message: "Bezit kluis + netwerk + uitvoeren — exfiltratie-risico. Beperk een van deze.".to_string(),
                    severity: Risk::High,
                });
            }
            if !a.verified && a.caps.contains(&Capability::Vault) {
                recs.push(Recommendation {
                    app: a.app.clone(),
                    message: "Niet-geverifieerde app met sleutelkluis-toegang. Trek de toegang in.".to_string(),
                    severity: Risk::High,
                });
            }
            if !a.sandboxed {
                recs.push(Recommendation {
                    app: a.app.clone(),
                    message: "Draait buiten de sandbox. Zet de app in een EuroGuard-sandbox.".to_string(),
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
        // Lichte app: scherm + geluid → laag.
        let viewer = AppPermissions::new("EuroMedia")
            .with(Capability::Display)
            .with(Capability::Audio)
            .with(Capability::FileRead);
        assert_eq!(viewer.risk(), Risk::Low);

        // Zware, ongesandboxede, onverifieerde app → hoog.
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
        // Vault staat vóór Net vóór Display (gesorteerd op gevoeligheid).
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
        // combo + onverifieerde-kluis + buiten-sandbox = 3 aanbevelingen.
        assert_eq!(recs.len(), 3);
        assert!(recs.iter().any(|r| r.severity == Risk::High));
        assert_eq!(db.system_risk(), Risk::High);
    }

    #[test]
    fn capability_labels_and_weights() {
        assert_eq!(Capability::Vault.label(), "Sleutelkluis");
        assert!(Capability::Vault.weight() > Capability::Display.weight());
        assert_eq!(Risk::High.label(), "Hoog");
    }
}
