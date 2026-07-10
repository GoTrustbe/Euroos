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

/// **3D-1: real TPM-sealed unseal, only on an untampered boot.** This replaces
/// the earlier *software* PCR-seal (a SHA-256 KDF that still kept the master key
/// in kernel RAM). Here the vault master — and, wired the same way, the FDE key
/// — is sealed **inside the TPM** under a `PolicyPCR` over the measured-boot PCR
/// (`TPM2_Create`); the plaintext key is never derivable from RAM.
///
/// Proves, on a live (emulated) TPM: (1) same boot state → the TPM releases the
/// exact key (`TPM2_Unseal`); (2) a **tampered boot** (the PCR gets extended with
/// a different measurement) → the TPM **itself refuses** (`TPM_RC_POLICY_FAIL`),
/// fail-closed in hardware, not in software; (3) with no TPM the key simply stays
/// sealed — there is no software fallback that would hand the key over.
pub fn tpm_seal_selftest(master_key: [u8; 32]) {
    let from_tpm = crate::tpm::present();
    let pcr = crate::tpm::SEAL_PCR;

    if !from_tpm {
        crate::serial_println!(
            "[3d1] TPM-seal: no TPM present → FDE/vault master stays sealed (fail-closed; no software fallback hands the key over) — needs a TPM to exercise"
        );
        return;
    }

    // (1) Seal the master to the current measured-boot state, then unseal it.
    let sealed = crate::tpm::seal_to_pcr(pcr, &master_key);
    let unseal_ok = sealed
        .as_ref()
        .and_then(|(pv, pb)| crate::tpm::unseal_from_pcr(pcr, pv, pb))
        .map(|k| k.len() == 32 && k[..] == master_key[..])
        .unwrap_or(false);

    // (2) Tamper the boot state: extend the PCR with a *different* measurement,
    // then attempt to unseal the SAME blob — the TPM must now refuse it.
    let tamper_digest = [0x9Eu8; 32];
    let tamper_extended = crate::tpm::extend_pcr(pcr, &tamper_digest);
    let tamper_denied = sealed
        .as_ref()
        .map(|(pv, pb)| crate::tpm::unseal_from_pcr(pcr, pv, pb).is_none())
        .unwrap_or(false);

    let ok = sealed.is_some() && unseal_ok && tamper_extended && tamper_denied;
    crate::serial_println!(
        "[3d1] TPM-seal (vault master sealed INSIDE the TPM to PCR{pcr}, real TPM2 Create/Load/Unseal): sealed={}, same-boot-unseal={unseal_ok}, tamper-extend-OK={tamper_extended}, tampered-boot-REFUSED-by-TPM={tamper_denied} → {}",
        sealed.is_some(),
        if ok { "OK (key released only on an untampered boot, hardware-enforced) ✓" } else { "FAILED" }
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
