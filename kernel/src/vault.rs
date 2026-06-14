//! Kernel side of **EuroVault** (plan U): a capability-gated secrets store with
//! a (TPM-generated, K3/O1-sealable) master key. Secrets are stored
//! encrypted; reading requires the right capability; every access goes to
//! the P3 audit trail.

use alloc::string::String;
use alloc::vec::Vec;

use eurovault::{Vault, VaultError};
use spin::Mutex;

/// The capability that grants read access to DB secrets (demo). In production this comes
/// from a EuroPol policy (X) / per-app capability grant.
pub const CAP_DB_ACCESS: u64 = 1 << 10;

static VAULT: Mutex<Option<Vault>> = Mutex::new(None);
static MASTER: Mutex<[u8; 32]> = Mutex::new([0u8; 32]);

/// Read a secret — capability-gated. Logs every (successful and denied) access
/// to the audit trail (P3).
pub fn get(label: &str, caller_caps: u64) -> Result<Vec<u8>, VaultError> {
    let guard = VAULT.lock();
    let v = guard.as_ref().ok_or(VaultError::NotFound)?;
    let r = v.get(label, caller_caps);
    match &r {
        Ok(_) => crate::audit::record(crate::audit::Event::Login, "vault-get (allowed)"),
        Err(VaultError::PermissionDenied) => crate::audit::record(crate::audit::Event::CapDenied, "vault-get (no cap)"),
        _ => {}
    }
    r
}

/// Boot self-test: build a vault with a TPM master key, prove the capability
/// gate and the seal/unseal roundtrip (tamper-evident).
pub fn selftest(master_key: [u8; 32], from_tpm: bool) {
    *MASTER.lock() = master_key;
    let mut v = Vault::new();
    v.set("db-password", b"euro-s3cr3t", CAP_DB_ACCESS);
    v.set("tls-key", b"-----BEGIN PRIVATE KEY-----", CAP_DB_ACCESS);

    // (1) Reading with the right cap → the value; without cap → EPERM (even if you know the label).
    let with_cap = v.get("db-password", CAP_DB_ACCESS).map(|d| d == b"euro-s3cr3t").unwrap_or(false);
    let without_cap = v.get("db-password", 0) == Err(VaultError::PermissionDenied);

    // (2) Seal → an encrypted blob that contains NO plaintext; then unseal again.
    // Fresh nonce per seal (audit M1): nonce reuse under the same master key
    // breaks ChaCha20-Poly1305. Pull it from the TPM RNG; fallback = a monotonic counter.
    let nonce = match crate::tpm::get_random(12) {
        Some(b) => {
            let mut n = [0u8; 12];
            n.copy_from_slice(&b[..12]);
            n
        }
        None => {
            static SEAL_CTR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
            let c = SEAL_CTR.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            let mut n = [0u8; 12];
            n[..8].copy_from_slice(&c.to_le_bytes());
            n
        }
    };
    let sealed = v.seal(&master_key, &nonce);
    let no_plaintext = sealed.as_ref().map(|b| !b.windows(11).any(|w| w == b"euro-s3cr3t")).unwrap_or(false);
    let unsealed_ok = sealed
        .as_ref()
        .ok()
        .and_then(|b| Vault::unseal(b, &master_key).ok())
        .and_then(|v2| v2.get("db-password", CAP_DB_ACCESS).ok())
        .map(|d| d == b"euro-s3cr3t")
        .unwrap_or(false);
    // (3) A wrong master key does NOT unseal (Poly1305 tag).
    let wrong_key_fails = sealed
        .as_ref()
        .ok()
        .map(|b| Vault::unseal(b, &[0u8; 32]).is_err())
        .unwrap_or(false);

    let ok = with_cap && without_cap && no_plaintext && unsealed_ok && wrong_key_fails;
    crate::serial_println!(
        "[u] EuroVault: {} secrets, master-from-TPM={from_tpm}, read-with-cap={with_cap}, read-without-cap-denied={without_cap}, blob-without-plaintext={no_plaintext}, unseal-OK={unsealed_ok}, wrong-key-fails={wrong_key_fails} → {}",
        v.len(),
        if ok { "OK (capability-gated, encrypted, tamper-evident) ✓" } else { "FAILED" }
    );
    *VAULT.lock() = Some(v);
}

/// **AF / Zero-Trust PCR-seal boot self-test** — bind a secret to the measured-
/// boot state. Reads the real PCR (16, extended by O1), seals a vault
/// PCR-bound, and proves: same PCRs → opens; a TAMPERED boot (different
/// PCR digest) → denied; and the PCR-bound blob does NOT open with the bare master
/// key (the binding is real, not cosmetic). This way e.g. the FDE master opens only on a
/// non-tampered system.
pub fn pcr_seal_selftest(master_key: [u8; 32]) {
    // The measured-boot measurement: PCR 16 (which the O1 self-test extends; fallback = synthetic).
    let pcr = crate::tpm::read_pcr(16).unwrap_or([0x11u8; 32]);
    let from_tpm = crate::tpm::present();

    let mut v = Vault::new();
    v.set("fde-master", b"disk-key-material", CAP_DB_ACCESS);

    let nonce = match crate::tpm::get_random(12) {
        Some(b) => {
            let mut n = [0u8; 12];
            n.copy_from_slice(&b[..12]);
            n
        }
        None => [0x5Au8; 12],
    };
    let sealed = v.seal_to_pcr(&master_key, &pcr, &nonce);

    // Same PCR state → unseals the secret.
    let good = sealed
        .as_ref()
        .ok()
        .and_then(|b| Vault::unseal_from_pcr(b, &master_key, &pcr).ok())
        .and_then(|v2| v2.get("fde-master", CAP_DB_ACCESS).ok())
        .map(|d| d == b"disk-key-material")
        .unwrap_or(false);

    // Tampered boot: one PCR byte different → unseal denied.
    let mut tampered = pcr;
    tampered[0] ^= 0x01;
    let tamper_denied = sealed
        .as_ref()
        .ok()
        .map(|b| Vault::unseal_from_pcr(b, &master_key, &tampered).is_err())
        .unwrap_or(false);

    // The PCR-bound blob does NOT open with the bare master (binding is real).
    let binding_real = sealed.as_ref().ok().map(|b| Vault::unseal(b, &master_key).is_err()).unwrap_or(false);

    let ok = good && tamper_denied && binding_real;
    crate::serial_println!(
        "[af-seal] PCR-seal (secret bound to measured boot, PCR16-from-TPM={from_tpm}): same-PCR-opens={good}, tampered-boot-denied={tamper_denied}, binding-real(bare-master-fails)={binding_real} → {}",
        if ok { "OK (FDE/vault-master opens only on a non-tampered system) ✓" } else { "FAILED" }
    );
}

/// `vault` shell command: list (labels + cap requirement, never values) or get (cap-gated).
pub fn shell(args: &str, caller_caps: u64) -> Vec<String> {
    let mut a = args.split_whitespace();
    match a.next() {
        Some("get") => match a.next() {
            Some(label) => match get(label, caller_caps) {
                Ok(_) => alloc::vec![alloc::format!("'{label}': [value released to process with the right capability]")],
                Err(VaultError::PermissionDenied) => alloc::vec![alloc::format!("'{label}': EPERM — this session does not have CAP_DB_ACCESS")],
                Err(e) => alloc::vec![alloc::format!("'{label}': {e:?}")],
            },
            None => alloc::vec![String::from("usage: vault get <label>")],
        },
        _ => {
            let guard = VAULT.lock();
            match guard.as_ref() {
                Some(v) => {
                    let mut out = alloc::vec![alloc::format!("vault ({} secrets — labels + cap requirement, NEVER values):", v.len())];
                    for (label, caps) in v.list() {
                        out.push(alloc::format!("  {:<16} read_caps={:#x}", label, caps));
                    }
                    out.push(String::from("commands: vault | vault get <label>"));
                    out
                }
                None => alloc::vec![String::from("vault: not initialized")],
            }
        }
    }
}
