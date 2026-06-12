//! Kernel-zijde van **EuroCA** (plan O3): een soevereine lokale certificaat-
//! autoriteit. Bij boot zetten we een (TPM-geseede) wortel-CA op, geven we een
//! dienstcertificaat uit en bewijzen we de volledige keten — uitgifte, verificatie,
//! revocatie. De host-geteste kern leeft in [`euroca`]; hier draait hij live.

use alloc::string::String;
use alloc::vec::Vec;

use euroca::{CertAuthority, Csr};
use spin::Mutex;

const YEAR: u64 = 365 * 24 * 3600;

static ROOT_FPR: Mutex<Option<String>> = Mutex::new(None);

/// Boot-zelftest: wortel-CA → dienstcertificaat uitgeven → verifiëren → intrekken →
/// verificatie faalt. `seed` is 32 bytes (van de TPM-RNG); `now` de echte wandklok.
pub fn selftest(seed: [u8; 32], from_tpm: bool, now: u64) {
    // De geldigheid moet `now` omvatten; gebruik een ruim venster rond de wandklok.
    let nb = now.saturating_sub(YEAR);
    let mut ca = CertAuthority::new_root("EuroCA Root (EuroOS)", seed, nb, now + 10 * YEAR);
    *ROOT_FPR.lock() = Some(ca.cert.fingerprint());

    // Een dienst vraagt een certificaat aan (eigen sleutel uit een tweede seed).
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
        "[ca] EuroCA: wortel-CA (seed-van-TPM={from_tpm}), dienst-cert 'vpn.euro-os.eu' uitgegeven+geverifieerd={verified}, na-revocatie-geweigerd={revoked_blocks}, root-fpr {}… → {}",
        &ca.cert.fingerprint()[..16],
        if ok { "OK (soevereine lokale certificaatautoriteit) ✓" } else { "MISLUKT" }
    );
}

/// `euroca`-shellcommando: toon de wortel-CA-status.
pub fn shell() -> Vec<String> {
    match &*ROOT_FPR.lock() {
        Some(fpr) => alloc::vec![
            String::from("EuroCA — soevereine lokale certificaatautoriteit (Ed25519 + SHA-256)"),
            alloc::format!("  wortel-CA vingerafdruk: {fpr}"),
            String::from("  geeft certificaten uit voor diensten/gebruikers/agents; ketenverificatie + revocatie"),
            String::from("  geen afhankelijkheid van een buitenlandse CA-hiërarchie — soeverein vertrouwensanker"),
        ],
        None => alloc::vec![String::from("EuroCA: nog niet geïnitialiseerd")],
    }
}
