//! Kernel-zijde van **EuroPol** (plan X): laad een declaratieve policy, compileer 'm
//! naar een EuroGuard-capability-masker, en dwing 'm af. Policy-violations gaan naar
//! het append-only audit-spoor (P3). Toont hoe menselijk beleid ("firefox mag geen
//! systeembestanden wijzigen") één-op-één een capability-resultaat wordt.

use alloc::string::String;
use alloc::vec::Vec;

use europol::{Decision, Policy};
use spin::Mutex;

/// De ingebakken voorbeeld-policy (zou in `/etc/europol/*.policy.toml` staan).
const FIREFOX_POLICY: &str = r#"
name = "firefox"
[allow]
capabilities = ["CAP_NET", "CAP_FILE", "CAP_CONSOLE"]
paths = ["/home/user", "/tmp"]
[deny]
capabilities = ["CAP_IMMUTABLE_ADMIN"]
paths = ["/etc", "/boot", "/sys"]
log_denied = true
"#;

static POLICY: Mutex<Option<Policy>> = Mutex::new(None);

/// Bereken het effectieve capability-masker voor een app onder de actieve policy.
/// Zonder policy: ongewijzigd. Met policy: `(base|allow) & !deny` — deny wint.
pub fn effective_caps(base: u64) -> u64 {
    match POLICY.lock().as_ref() {
        Some(p) => p.effective_caps(base),
        None => base,
    }
}

/// Boot-zelftest: parse de policy, bewijs de capability-reductie + pad-regel, en log
/// een violation naar P3.
pub fn selftest() {
    let p = europol::parse(FIREFOX_POLICY);
    let all = europol::CAP_CONSOLE | europol::CAP_PROC_INFO | europol::CAP_FILE | europol::CAP_NET | europol::CAP_IMMUTABLE_ADMIN;
    let eff = p.effective_caps(all);

    let cap_denied = p.check_cap(europol::CAP_IMMUTABLE_ADMIN) == Decision::Deny;
    let path_denied = p.check_path("/etc/shadow") == Decision::Deny;
    let net_allowed = p.check_cap(europol::CAP_NET) == Decision::Allow;
    // Een policy-violation (firefox vraagt een geweigerde cap) → audit-spoor (P3).
    if cap_denied && p.log_denied {
        crate::audit::record(crate::audit::Event::CapDenied, "firefox vroeg CAP_IMMUTABLE_ADMIN (europol-deny)");
    }

    let ok = cap_denied && path_denied && net_allowed && (eff & europol::CAP_IMMUTABLE_ADMIN == 0) && (eff & europol::CAP_NET != 0);
    crate::serial_println!(
        "[x] EuroPol: policy '{}', caps {:#07b}→{:#07b} (CAP_IMMUTABLE_ADMIN ontnomen, CAP_NET behouden), /etc geweigerd={path_denied}, violation→P3-audit → {}",
        p.name, all, eff,
        if ok { "OK (beleid afgedwongen als capabilities) ✓" } else { "MISLUKT" }
    );
    *POLICY.lock() = Some(p);
}

/// `europol`-shellcommando: toon/explain het effectieve beleid.
pub fn shell(args: &str) -> Vec<String> {
    let guard = POLICY.lock();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return alloc::vec![String::from("europol: geen policy geladen")],
    };
    let mut a = args.split_whitespace();
    match a.next() {
        Some("explain") => {
            let cap = a.next().and_then(europol::cap_bit);
            match cap {
                Some(c) => alloc::vec![p.explain_cap(c)],
                None => alloc::vec![String::from("gebruik: europol explain <CAP_NAAM>")],
            }
        }
        _ => {
            let mut v = alloc::vec![alloc::format!("policy '{}' (effectief beleid):", p.name)];
            for bit in [europol::CAP_CONSOLE, europol::CAP_PROC_INFO, europol::CAP_FILE, europol::CAP_NET, europol::CAP_IMMUTABLE_ADMIN] {
                let d = if p.check_cap(bit) == Decision::Allow { "toegestaan" } else { "GEWEIGERD" };
                v.push(alloc::format!("  {:<20} {}", europol::cap_name(bit), d));
            }
            v.push(String::from("commando's: europol | europol explain <CAP_NAAM>"));
            v
        }
    }
}
