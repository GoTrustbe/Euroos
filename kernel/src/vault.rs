//! Kernel-zijde van **EuroVault** (plan U): een capability-gated secrets-store met
//! een (TPM-gegenereerde, met K3/O1 sealbare) master-sleutel. Secrets worden
//! versleuteld bewaard; lezen vereist de juiste capability; elke toegang gaat naar
//! het P3-audit-spoor.

use alloc::string::String;
use alloc::vec::Vec;

use eurovault::{Vault, VaultError};
use spin::Mutex;

/// De capability die leesrecht geeft op DB-secrets (demo). In productie komt dit uit
/// een EuroPol-policy (X) / per-app-capability-grant.
pub const CAP_DB_ACCESS: u64 = 1 << 10;

static VAULT: Mutex<Option<Vault>> = Mutex::new(None);
static MASTER: Mutex<[u8; 32]> = Mutex::new([0u8; 32]);

/// Lees een secret — capability-gated. Logt elke (geslaagde én geweigerde) toegang
/// naar het audit-spoor (P3).
pub fn get(label: &str, caller_caps: u64) -> Result<Vec<u8>, VaultError> {
    let guard = VAULT.lock();
    let v = guard.as_ref().ok_or(VaultError::NotFound)?;
    let r = v.get(label, caller_caps);
    match &r {
        Ok(_) => crate::audit::record(crate::audit::Event::Login, "vault-get (toegestaan)"),
        Err(VaultError::PermissionDenied) => crate::audit::record(crate::audit::Event::CapDenied, "vault-get (geen cap)"),
        _ => {}
    }
    r
}

/// Boot-zelftest: bouw een vault met een TPM-master-sleutel, bewijs de capability-
/// poort en de seal/unseal-roundtrip (tamper-evident).
pub fn selftest(master_key: [u8; 32], from_tpm: bool) {
    *MASTER.lock() = master_key;
    let mut v = Vault::new();
    v.set("db-password", b"euro-s3cr3t", CAP_DB_ACCESS);
    v.set("tls-key", b"-----BEGIN PRIVATE KEY-----", CAP_DB_ACCESS);

    // (1) Lezen met de juiste cap → de waarde; zonder cap → EPERM (ook al ken je 't label).
    let with_cap = v.get("db-password", CAP_DB_ACCESS).map(|d| d == b"euro-s3cr3t").unwrap_or(false);
    let without_cap = v.get("db-password", 0) == Err(VaultError::PermissionDenied);

    // (2) Verzegelen → een versleutelde blob die GEEN plaintext bevat; weer ontzegelen.
    // Verse nonce per seal (audit M1): nonce-hergebruik onder dezelfde master-sleutel
    // breekt ChaCha20-Poly1305. Trek 'm uit de TPM-RNG; vangnet = een monotone teller.
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
    // (3) Een verkeerde master-sleutel ontzegelt NIET (Poly1305-tag).
    let wrong_key_fails = sealed
        .as_ref()
        .ok()
        .map(|b| Vault::unseal(b, &[0u8; 32]).is_err())
        .unwrap_or(false);

    let ok = with_cap && without_cap && no_plaintext && unsealed_ok && wrong_key_fails;
    crate::serial_println!(
        "[u] EuroVault: {} secrets, master-van-TPM={from_tpm}, lezen-met-cap={with_cap}, lezen-zonder-cap-geweigerd={without_cap}, blob-zonder-plaintext={no_plaintext}, unseal-OK={unsealed_ok}, verkeerde-sleutel-faalt={wrong_key_fails} → {}",
        v.len(),
        if ok { "OK (capability-gated, versleuteld, tamper-evident) ✓" } else { "MISLUKT" }
    );
    *VAULT.lock() = Some(v);
}

/// **AF / Zero-Trust PCR-seal boot-zelftest** — bind een geheim aan de measured-
/// boot-toestand. Leest de échte PCR (16, door O1 ge-extend), verzegelt een vault
/// PCR-gebonden, en bewijst: zelfde PCR's → opent; een GEMANIPULEERDE boot (andere
/// PCR-digest) → geweigerd; en de PCR-gebonden blob opent NIET met de kale master
/// (de binding is echt, niet cosmetisch). Zo opent bv. de FDE-master enkel op een
/// niet-gemanipuleerd systeem.
pub fn pcr_seal_selftest(master_key: [u8; 32]) {
    // De measured-boot-meting: PCR 16 (die de O1-zelftest extend; vangnet = synthetisch).
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

    // Zelfde PCR-toestand → ontzegelt het geheim.
    let good = sealed
        .as_ref()
        .ok()
        .and_then(|b| Vault::unseal_from_pcr(b, &master_key, &pcr).ok())
        .and_then(|v2| v2.get("fde-master", CAP_DB_ACCESS).ok())
        .map(|d| d == b"disk-key-material")
        .unwrap_or(false);

    // Gemanipuleerde boot: één PCR-byte anders → ontzegelen geweigerd.
    let mut tampered = pcr;
    tampered[0] ^= 0x01;
    let tamper_denied = sealed
        .as_ref()
        .ok()
        .map(|b| Vault::unseal_from_pcr(b, &master_key, &tampered).is_err())
        .unwrap_or(false);

    // De PCR-gebonden blob opent NIET met de kale master (binding is echt).
    let binding_real = sealed.as_ref().ok().map(|b| Vault::unseal(b, &master_key).is_err()).unwrap_or(false);

    let ok = good && tamper_denied && binding_real;
    crate::serial_println!(
        "[af-seal] PCR-seal (geheim gebonden aan measured boot, PCR16-van-TPM={from_tpm}): zelfde-PCR-opent={good}, gemanipuleerde-boot-geweigerd={tamper_denied}, binding-echt(kale-master-faalt)={binding_real} → {}",
        if ok { "OK (FDE/vault-master opent enkel op een niet-gemanipuleerd systeem) ✓" } else { "MISLUKT" }
    );
}

/// `vault`-shellcommando: list (labels + cap-eis, nooit waarden) of get (cap-gated).
pub fn shell(args: &str, caller_caps: u64) -> Vec<String> {
    let mut a = args.split_whitespace();
    match a.next() {
        Some("get") => match a.next() {
            Some(label) => match get(label, caller_caps) {
                Ok(_) => alloc::vec![alloc::format!("'{label}': [waarde vrijgegeven aan proces met de juiste capability]")],
                Err(VaultError::PermissionDenied) => alloc::vec![alloc::format!("'{label}': EPERM — deze sessie heeft niet CAP_DB_ACCESS")],
                Err(e) => alloc::vec![alloc::format!("'{label}': {e:?}")],
            },
            None => alloc::vec![String::from("gebruik: vault get <label>")],
        },
        _ => {
            let guard = VAULT.lock();
            match guard.as_ref() {
                Some(v) => {
                    let mut out = alloc::vec![alloc::format!("vault ({} secrets — labels + cap-eis, NOOIT waarden):", v.len())];
                    for (label, caps) in v.list() {
                        out.push(alloc::format!("  {:<16} read_caps={:#x}", label, caps));
                    }
                    out.push(String::from("commando's: vault | vault get <label>"));
                    out
                }
                None => alloc::vec![String::from("vault: niet geïnitialiseerd")],
            }
        }
    }
}
