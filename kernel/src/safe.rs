//! Kernel-zijde van **EuroSafe** (Sprint AC-1): het capability-dashboard.
//! Bij boot bewijzen we de risico-scoring + aanbevelingen op een voorbeeld-set
//! apps — het zichtbare gezicht van EuroGuard. Host-geteste kern: [`eurosafe`].

use crate::serial_println;
use eurosafe::{AppPermissions, AuditEvent, Capability, Dashboard, Risk};

/// Boot-zelftest: bouw een dashboard, classificeer risico's, geef aanbevelingen.
pub fn selftest() {
    let mut db = Dashboard::new();
    db.add_app(
        AppPermissions::new("EuroMedia")
            .with(Capability::Display)
            .with(Capability::FileRead),
    );
    db.add_app(
        AppPermissions::new("EuroMail")
            .with(Capability::Net)
            .with(Capability::Vault)
            .with(Capability::FileRead),
    );
    // Gevaarlijke, ongesandboxede, onverifieerde agent.
    db.add_app(
        AppPermissions::new("rogue.bin")
            .with(Capability::Vault)
            .with(Capability::Net)
            .with(Capability::Exec)
            .sandboxed(false)
            .verified(false),
    );
    db.add_event(AuditEvent { app: "rogue.bin".into(), cap: Capability::Vault, allowed: false });

    let high = db.high_risk_apps().len();
    let caps = db.caps_in_use();
    let top_cap = caps.first().map(|(c, _)| c.label()).unwrap_or("-");
    let denied = db.denied_count();
    let recs = db.recommendations();
    let sys = db.system_risk();

    let ok = high == 1
        && top_cap == "Sleutelkluis"
        && denied == 1
        && recs.len() >= 2
        && sys == Risk::High;

    serial_println!(
        "[sf] EuroSafe: {} apps, hoog-risico={}, top-cap={}, geweigerd={}, aanbevelingen={}, systeemrisico={} {}",
        db.apps.len(),
        high,
        top_cap,
        denied,
        recs.len(),
        sys.label(),
        if ok { "✓" } else { "✗ FOUT" }
    );
}
