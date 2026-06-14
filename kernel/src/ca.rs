//! Kernel side of **EuroCA** (plan O3): a sovereign local certificate
//! authority. At boot we set up a (TPM-seeded) root CA, issue a
//! service certificate and prove the full chain — issuance, verification,
//! revocation. The host-tested core lives in [`euroca`]; here it runs live.

use alloc::string::String;
use alloc::vec::Vec;

use euroca::{CertAuthority, Csr};
use spin::Mutex;

const YEAR: u64 = 365 * 24 * 3600;

static ROOT_FPR: Mutex<Option<String>> = Mutex::new(None);

/// Boot self-test: root CA → issue service certificate → verify → revoke →
/// verification fails. `seed` is 32 bytes (from the TPM RNG); `now` the real wall clock.
pub fn selftest(seed: [u8; 32], from_tpm: bool, now: u64) {
    // The validity must include `now`; use a wide window around the wall clock.
    let nb = now.saturating_sub(YEAR);
    let mut ca = CertAuthority::new_root("EuroCA Root (EuroOS)", seed, nb, now + 10 * YEAR);
    *ROOT_FPR.lock() = Some(ca.cert.fingerprint());

    // A service requests a certificate (its own key from a second seed).
    let svc_seed = {
        let mut s = seed;
        s[0] ^= 0xA5;
        s
    };
    let svc_key = ed25519_dalek::SigningKey::from_bytes(&svc_seed).verifying_key().to_bytes();
    let csr = Csr { subject: String::from("vpn.euro-os.eu"), subject_key: svc_key, is_ca: false };
    let cert = ca.issue(&csr, nb, now + YEAR);

    let verified = ca.verify_issued(&cert, now).is_ok();
    ca.revoke(cert.serial);
    let revoked_blocks = matches!(ca.verify_issued(&cert, now), Err(euroca::CertError::Revoked));

    let ok = verified && revoked_blocks;
    crate::serial_println!(
        "[ca] EuroCA: root CA (seed-from-TPM={from_tpm}), service cert 'vpn.euro-os.eu' issued+verified={verified}, refused-after-revocation={revoked_blocks}, root-fpr {}… → {}",
        &ca.cert.fingerprint()[..16],
        if ok { "OK (sovereign local certificate authority) ✓" } else { "FAILED" }
    );
}

/// `euroca` shell command: show the root CA status.
pub fn shell() -> Vec<String> {
    match &*ROOT_FPR.lock() {
        Some(fpr) => alloc::vec![
            String::from("EuroCA — sovereign local certificate authority (Ed25519 + SHA-256)"),
            alloc::format!("  root CA fingerprint: {fpr}"),
            String::from("  issues certificates for services/users/agents; chain verification + revocation"),
            String::from("  no dependency on a foreign CA hierarchy — sovereign trust anchor"),
        ],
        None => alloc::vec![String::from("EuroCA: not yet initialized")],
    }
}
