//! EuroVault — een **capability-gated, versleutelde secrets-store** (plan U).
//!
//! TPM (O1) levert hardware-sleutelopslag, capabilities (EuroGuard) regelen toegang.
//! EuroVault is de laag ertussen: een store die secrets (DB-wachtwoorden, API-keys)
//! koppelt aan een **`read_caps`-capability-eis** en ze **versleuteld** (ChaCha20-
//! Poly1305) op schijf bewaart, ontsleuteld met een master-sleutel die (met K3/O1)
//! TPM-sealed is. Een proces vraagt een secret via een capability-gated call; zonder
//! de juiste capability volgt `EPERM`. Secret-bytes worden bij drop gewist
//! (`zeroize`). Elke toegang hoort in het P3-audit-spoor. Pure `no_std` → host-getest.

#![cfg_attr(not(test), no_std)]
// Eén gericht stukje `unsafe`: het volatile-wissen van secret-bytes bij drop, zodat
// de compiler het niet weg-optimaliseert (echte zeroize). Verder géén unsafe.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultError {
    NotFound,
    PermissionDenied,
    Decrypt,
    Corrupt,
}

/// Eén secret. De waarde wordt bij drop met nullen overschreven (anti-forensisch).
pub struct Secret {
    pub label: String,
    value: Vec<u8>,
    pub read_caps: u64, // het capability-masker dat leesrecht geeft (0 = altijd)
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Wis de geheime bytes — ze mogen niet in vrijgegeven geheugen achterblijven.
        for b in self.value.iter_mut() {
            unsafe { core::ptr::write_volatile(b, 0) };
        }
    }
}

/// De secrets-store. Versleuteld te (de)serialiseren met een 32-byte master-sleutel.
#[derive(Default)]
pub struct Vault {
    secrets: Vec<Secret>,
}

impl Vault {
    pub fn new() -> Self {
        Vault { secrets: Vec::new() }
    }

    /// Plaats/overschrijf een secret met z'n capability-eis.
    pub fn set(&mut self, label: &str, value: &[u8], read_caps: u64) {
        if let Some(s) = self.secrets.iter_mut().find(|s| s.label == label) {
            s.value.clear();
            s.value.extend_from_slice(value);
            s.read_caps = read_caps;
        } else {
            self.secrets.push(Secret {
                label: String::from(label),
                value: value.to_vec(),
                read_caps,
            });
        }
    }

    /// Lees een secret — ALLEEN als `caller_caps` de vereiste `read_caps` bevat.
    /// Geeft een kopie (de aanroeper wist 'm zelf na gebruik).
    pub fn get(&self, label: &str, caller_caps: u64) -> Result<Vec<u8>, VaultError> {
        let s = self.secrets.iter().find(|s| s.label == label).ok_or(VaultError::NotFound)?;
        if s.read_caps != 0 && (caller_caps & s.read_caps) != s.read_caps {
            return Err(VaultError::PermissionDenied);
        }
        Ok(s.value.clone())
    }

    /// De labels + hun capability-eis (NOOIT de waarden) — voor `vault list`.
    pub fn list(&self) -> Vec<(String, u64)> {
        self.secrets.iter().map(|s| (s.label.clone(), s.read_caps)).collect()
    }

    pub fn len(&self) -> usize {
        self.secrets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// Serialiseer (platte bytes) → label/​caps/​value per secret.
    fn serialize(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&(self.secrets.len() as u32).to_le_bytes());
        for s in &self.secrets {
            let lb = s.label.as_bytes();
            b.extend_from_slice(&(lb.len() as u32).to_le_bytes());
            b.extend_from_slice(lb);
            b.extend_from_slice(&s.read_caps.to_le_bytes());
            b.extend_from_slice(&(s.value.len() as u32).to_le_bytes());
            b.extend_from_slice(&s.value);
        }
        b
    }

    fn deserialize(data: &[u8]) -> Result<Vault, VaultError> {
        if data.len() < 4 {
            return Err(VaultError::Corrupt);
        }
        let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let mut p = 4;
        let mut v = Vault::new();
        let rd_u32 = |d: &[u8], p: usize| -> Option<u32> {
            d.get(p..p + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        };
        for _ in 0..count {
            let ll = rd_u32(data, p).ok_or(VaultError::Corrupt)? as usize;
            p += 4;
            let label = String::from_utf8_lossy(data.get(p..p + ll).ok_or(VaultError::Corrupt)?).into_owned();
            p += ll;
            let caps = {
                let s = data.get(p..p + 8).ok_or(VaultError::Corrupt)?;
                u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
            };
            p += 8;
            let vl = rd_u32(data, p).ok_or(VaultError::Corrupt)? as usize;
            p += 4;
            let value = data.get(p..p + vl).ok_or(VaultError::Corrupt)?.to_vec();
            p += vl;
            v.secrets.push(Secret { label, value, read_caps: caps });
        }
        Ok(v)
    }

    /// **Verzegel** de hele vault tot een versleutelde blob (ChaCha20-Poly1305) met
    /// `master_key` (256-bit, idealiter TPM-sealed) + een 12-byte `nonce`. De blob =
    /// `nonce ‖ ciphertext+tag` → tamper-evident (de tag detecteert wijziging).
    pub fn seal(&self, master_key: &[u8; 32], nonce: &[u8; 12]) -> Result<Vec<u8>, VaultError> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(master_key));
        let ct = cipher
            .encrypt(Nonce::from_slice(nonce), self.serialize().as_slice())
            .map_err(|_| VaultError::Decrypt)?;
        let mut out = Vec::with_capacity(12 + ct.len());
        out.extend_from_slice(nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// **Ontzegel** een blob met de master-sleutel. Faalt (`Decrypt`) bij een verkeerde
    /// sleutel of een gemanipuleerde blob (Poly1305-tag mismatch).
    pub fn unseal(blob: &[u8], master_key: &[u8; 32]) -> Result<Vault, VaultError> {
        if blob.len() < 12 {
            return Err(VaultError::Corrupt);
        }
        let cipher = ChaCha20Poly1305::new(Key::from_slice(master_key));
        let pt = cipher
            .decrypt(Nonce::from_slice(&blob[..12]), &blob[12..])
            .map_err(|_| VaultError::Decrypt)?;
        Vault::deserialize(&pt)
    }

    /// **Verzegel gebonden aan de boot-meting (PCR-seal, AF / Zero-Trust).** De
    /// effectieve sleutel is `SHA256("EuroVault-PCR-seal-v1" ‖ master ‖ pcr_digest)`:
    /// de verzegeling is zo cryptografisch gebonden aan de measured-boot-toestand.
    /// Op een GEMANIPULEERD systeem verschillen de PCR's → een andere afgeleide
    /// sleutel → de Poly1305-tag faalt en ontzegelen wordt geweigerd. Zo opent het
    /// geheim (bv. de FDE-master) enkel op een niet-gemanipuleerde boot.
    pub fn seal_to_pcr(&self, master_key: &[u8; 32], pcr_digest: &[u8; 32], nonce: &[u8; 12]) -> Result<Vec<u8>, VaultError> {
        let k = derive_pcr_key(master_key, pcr_digest);
        self.seal(&k, nonce)
    }

    /// Ontzegel een PCR-gebonden blob met de master-sleutel en de HUIDIGE PCR-meting.
    /// Komen de PCR's niet overeen met die bij het verzegelen (gewijzigde boot), dan
    /// faalt dit met `Decrypt` — het geheim blijft ontoegankelijk.
    pub fn unseal_from_pcr(blob: &[u8], master_key: &[u8; 32], current_pcr_digest: &[u8; 32]) -> Result<Vault, VaultError> {
        let k = derive_pcr_key(master_key, current_pcr_digest);
        Vault::unseal(blob, &k)
    }
}

/// KDF die de ontzegelsleutel aan (master, PCR-toestand) bindt. Domein-gescheiden
/// zodat de afgeleide sleutel nergens anders voor (her)gebruikt kan worden.
fn derive_pcr_key(master_key: &[u8; 32], pcr_digest: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"EuroVault-PCR-seal-v1");
    h.update(master_key);
    h.update(pcr_digest);
    let out = h.finalize();
    let mut k = [0u8; 32];
    k.copy_from_slice(&out);
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP_DB: u64 = 1 << 10;
    const CAP_OTHER: u64 = 1 << 11;

    #[test]
    fn capability_gated_read() {
        let mut v = Vault::new();
        v.set("db-password", b"s3cr3t", CAP_DB);
        // Met de juiste cap → de waarde.
        assert_eq!(v.get("db-password", CAP_DB | CAP_OTHER).unwrap(), b"s3cr3t");
        // Zonder de cap → EPERM, óók al ken je het label.
        assert_eq!(v.get("db-password", CAP_OTHER), Err(VaultError::PermissionDenied));
        // Onbekend label.
        assert_eq!(v.get("weg", CAP_DB), Err(VaultError::NotFound));
    }

    #[test]
    fn list_never_leaks_values() {
        let mut v = Vault::new();
        v.set("api-key", b"AKIA....", CAP_DB);
        let l = v.list();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].0, "api-key");
        assert_eq!(l[0].1, CAP_DB);
        // (list geeft enkel labels + caps — de waarden komen er niet uit)
    }

    #[test]
    fn seal_unseal_roundtrip() {
        let key = [0x33u8; 32];
        let nonce = [1u8; 12];
        let mut v = Vault::new();
        v.set("db-password", b"hunter2", CAP_DB);
        v.set("ssh-key", b"-----BEGIN-----", 0);
        let blob = v.seal(&key, &nonce).unwrap();
        // De blob bevat GEEN plaintext.
        assert!(!blob.windows(7).any(|w| w == b"hunter2"));
        let v2 = Vault::unseal(&blob, &key).unwrap();
        assert_eq!(v2.get("db-password", CAP_DB).unwrap(), b"hunter2");
        assert_eq!(v2.get("ssh-key", 0).unwrap(), b"-----BEGIN-----");
    }

    #[test]
    fn pcr_seal_only_unseals_on_matching_boot_state() {
        // AF / PCR-seal: een vault gebonden aan PCR-toestand A opent ALLEEN onder
        // diezelfde PCR's. Een gewijzigde boot (andere PCR-digest) → ontzegelen faalt.
        let master = [0x77u8; 32];
        let nonce = [9u8; 12];
        let pcr_good = [0xAAu8; 32]; // measured-boot-digest bij verzegelen
        let pcr_tampered = [0xABu8; 32]; // één bit anders = gewijzigde boot-keten

        let mut v = Vault::new();
        v.set("fde-master", b"disk-key-material", CAP_DB);
        let blob = v.seal_to_pcr(&master, &pcr_good, &nonce).unwrap();

        // Zelfde PCR's → ontzegelt.
        let ok = Vault::unseal_from_pcr(&blob, &master, &pcr_good).unwrap();
        assert_eq!(ok.get("fde-master", CAP_DB).unwrap(), b"disk-key-material");
        // Gemanipuleerde boot (andere PCR's) → geweigerd.
        assert_eq!(Vault::unseal_from_pcr(&blob, &master, &pcr_tampered).err(), Some(VaultError::Decrypt));
        // Juiste PCR's maar verkeerde master → ook geweigerd.
        assert_eq!(Vault::unseal_from_pcr(&blob, &[0u8; 32], &pcr_good).err(), Some(VaultError::Decrypt));
        // Een blob die PCR-gebonden is, opent NIET met de kale master (binding is echt).
        assert_eq!(Vault::unseal(&blob, &master).err(), Some(VaultError::Decrypt));
    }

    #[test]
    fn wrong_key_or_tamper_fails() {
        let key = [0x33u8; 32];
        let nonce = [2u8; 12];
        let mut v = Vault::new();
        v.set("x", b"geheim", 0);
        let mut blob = v.seal(&key, &nonce).unwrap();
        // Verkeerde sleutel → Decrypt-fout.
        assert_eq!(Vault::unseal(&blob, &[0u8; 32]).err(), Some(VaultError::Decrypt));
        // Eén byte flippen (tamper) → Poly1305 detecteert het.
        let n = blob.len();
        blob[n - 1] ^= 0xFF;
        assert_eq!(Vault::unseal(&blob, &key).err(), Some(VaultError::Decrypt));
    }
}
