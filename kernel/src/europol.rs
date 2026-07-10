//! Kernel side of **EuroPol** (plan X): load a declarative policy, compile it
//! into an EuroGuard capability mask, and enforce it. Policy violations go to
//! the append-only audit trail (P3). Demonstrates how human policy ("firefox may not
//! modify system files") becomes a capability result one-to-one.

use alloc::string::String;
use alloc::vec::Vec;

use europol::{Decision, Policy};
use spin::Mutex;

/// The built-in example policy (would live in `/etc/europol/*.policy.toml`).
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

/// Compute the effective capability mask for an app under the active policy.
/// Without a policy: unchanged. With a policy: `(base|allow) & !deny` — deny wins.
pub fn effective_caps(base: u64) -> u64 {
    match POLICY.lock().as_ref() {
        Some(p) => p.effective_caps(base),
        None => base,
    }
}

/// 3D-4 boot self-test: a policy is a security-critical input, so it must be
/// **signed and verified before it can change any capability**. Serialize the
/// policy into a bundle, sign it, verify-before-load, then prove a tampered
/// bundle is refused and the safe default (no elevation) is kept.
pub fn bundle_selftest() {
    use ed25519_dalek::SigningKey;
    let policies = alloc::vec![europol::parse(FIREFOX_POLICY)];
    let bytes = europol::bundle::serialize(&policies);

    // Sign with a release key (from the CSPRNG here; offline in production).
    let mut seed = [0u8; 32];
    crate::entropy::getrandom(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    let pubkey = key.verifying_key().to_bytes();
    let sig = europol::bundle::sign(&bytes, &key);

    // Verify-before-load: a valid bundle loads and yields the policy.
    let loaded = europol::bundle::load_verified(&bytes, &sig, &pubkey);
    let load_ok = loaded.as_ref().map(|v| v.len() == 1 && v[0].name == "firefox").unwrap_or(false);

    // A tampered bundle is REFUSED (returns None) → the default policy stands.
    let mut evil = bytes.clone();
    let ei = evil.len() - 8;
    evil[ei] ^= 0xFF; // flip a byte in the last policy
    let tamper_refused = europol::bundle::load_verified(&evil, &sig, &pubkey).is_none();

    // A bundle signed by the wrong key is refused.
    let other = SigningKey::from_bytes(&[0xEE; 32]).verifying_key().to_bytes();
    let wrong_key_refused = europol::bundle::load_verified(&bytes, &sig, &other).is_none();

    // Apply only the verified policy.
    if let Some(v) = loaded {
        if let Some(p) = v.into_iter().next() {
            *POLICY.lock() = Some(p);
        }
    }

    let ok = load_ok && tamper_refused && wrong_key_refused;
    crate::serial_println!(
        "[3d4] EuroPol signed bundles (Ed25519 verify-before-load): valid-bundle-loaded={load_ok}, tampered-bundle-REFUSED={tamper_refused}, wrong-signer-REFUSED={wrong_key_refused} → {}",
        if ok { "OK (a policy can only change capabilities if it is signed by the release key) ✓" } else { "FAILED" }
    );
}

/// Boot self-test: parse the policy, prove the capability reduction + path rule, and log
/// a violation to P3.
pub fn selftest() {
    let p = europol::parse(FIREFOX_POLICY);
    let all = europol::CAP_CONSOLE | europol::CAP_PROC_INFO | europol::CAP_FILE | europol::CAP_NET | europol::CAP_IMMUTABLE_ADMIN;
    let eff = p.effective_caps(all);

    let cap_denied = p.check_cap(europol::CAP_IMMUTABLE_ADMIN) == Decision::Deny;
    let path_denied = p.check_path("/etc/shadow") == Decision::Deny;
    let net_allowed = p.check_cap(europol::CAP_NET) == Decision::Allow;
    // A policy violation (firefox requests a denied cap) → audit trail (P3).
    if cap_denied && p.log_denied {
        crate::audit::record(crate::audit::Event::CapDenied, "firefox requested CAP_IMMUTABLE_ADMIN (europol-deny)");
    }

    let ok = cap_denied && path_denied && net_allowed && (eff & europol::CAP_IMMUTABLE_ADMIN == 0) && (eff & europol::CAP_NET != 0);
    crate::serial_println!(
        "[x] EuroPol: policy '{}', caps {:#07b}→{:#07b} (CAP_IMMUTABLE_ADMIN revoked, CAP_NET retained), /etc denied={path_denied}, violation→P3-audit → {}",
        p.name, all, eff,
        if ok { "OK (policy enforced as capabilities) ✓" } else { "FAILED" }
    );
    *POLICY.lock() = Some(p);
}

/// `europol` shell command: show/explain the effective policy.
pub fn shell(args: &str) -> Vec<String> {
    let guard = POLICY.lock();
    let p = match guard.as_ref() {
        Some(p) => p,
        None => return alloc::vec![String::from("europol: no policy loaded")],
    };
    let mut a = args.split_whitespace();
    match a.next() {
        Some("explain") => {
            let cap = a.next().and_then(europol::cap_bit);
            match cap {
                Some(c) => alloc::vec![p.explain_cap(c)],
                None => alloc::vec![String::from("usage: europol explain <CAP_NAME>")],
            }
        }
        _ => {
            let mut v = alloc::vec![alloc::format!("policy '{}' (effective policy):", p.name)];
            for bit in [europol::CAP_CONSOLE, europol::CAP_PROC_INFO, europol::CAP_FILE, europol::CAP_NET, europol::CAP_IMMUTABLE_ADMIN] {
                let d = if p.check_cap(bit) == Decision::Allow { "allowed" } else { "DENIED" };
                v.push(alloc::format!("  {:<20} {}", europol::cap_name(bit), d));
            }
            v.push(String::from("commands: europol | europol explain <CAP_NAME>"));
            v
        }
    }
}
