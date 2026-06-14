//! EuroVault — a **capability-gated, encrypted secrets store** (plan U).
//!
//! TPM (O1) provides hardware key storage, capabilities (EuroGuard) govern access.
//! EuroVault is the layer in between: a store that ties secrets (DB passwords, API keys)
//! to a **`read_caps` capability requirement** and keeps them **encrypted** (ChaCha20-
//! Poly1305) on disk, decrypted with a master key that (with K3/O1) is
//! TPM-sealed. A process requests a secret via a capability-gated call; without
//! the proper capability it gets `EPERM`. Secret bytes are wiped on drop
//! (`zeroize`). Every access belongs in the P3 audit trail. Pure `no_std` → host-tested.

#![cfg_attr(not(test), no_std)]
// One targeted piece of `unsafe`: the volatile wiping of secret bytes on drop, so
// the compiler does not optimize it away (real zeroize). Otherwise no unsafe.

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

/// One secret. The value is overwritten with zeros on drop (anti-forensic).
pub struct Secret {
    pub label: String,
    value: Vec<u8>,
    pub read_caps: u64, // the capability mask that grants read access (0 = always)
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Wipe the secret bytes — they must not remain in freed memory.
        for b in self.value.iter_mut() {
            unsafe { core::ptr::write_volatile(b, 0) };
        }
    }
}

/// The secrets store. (De)serialized encrypted with a 32-byte master key.
#[derive(Default)]
pub struct Vault {
    secrets: Vec<Secret>,
}

impl Vault {
    pub fn new() -> Self {
        Vault { secrets: Vec::new() }
    }

    /// Place/overwrite a secret with its capability requirement.
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

    /// Read a secret — ONLY if `caller_caps` contains the required `read_caps`.
    /// Returns a copy (the caller wipes it themselves after use).
    pub fn get(&self, label: &str, caller_caps: u64) -> Result<Vec<u8>, VaultError> {
        let s = self.secrets.iter().find(|s| s.label == label).ok_or(VaultError::NotFound)?;
        if s.read_caps != 0 && (caller_caps & s.read_caps) != s.read_caps {
            return Err(VaultError::PermissionDenied);
        }
        Ok(s.value.clone())
    }

    /// The labels + their capability requirement (NEVER the values) — for `vault list`.
    pub fn list(&self) -> Vec<(String, u64)> {
        self.secrets.iter().map(|s| (s.label.clone(), s.read_caps)).collect()
    }

    pub fn len(&self) -> usize {
        self.secrets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// Serialize (flat bytes) → label/​caps/​value per secret.
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

    /// **Seal** the whole vault into an encrypted blob (ChaCha20-Poly1305) with
    /// `master_key` (256-bit, ideally TPM-sealed) + a 12-byte `nonce`. The blob =
    /// `nonce ‖ ciphertext+tag` → tamper-evident (the tag detects modification).
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

    /// **Unseal** a blob with the master key. Fails (`Decrypt`) on a wrong
    /// key or a tampered blob (Poly1305 tag mismatch).
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

    /// **Seal bound to the boot measurement (PCR-seal, AF / Zero-Trust).** The
    /// effective key is `SHA256("EuroVault-PCR-seal-v1" ‖ master ‖ pcr_digest)`:
    /// the seal is thus cryptographically bound to the measured-boot state.
    /// On a TAMPERED system the PCRs differ → a different derived
    /// key → the Poly1305 tag fails and unsealing is refused. This way the
    /// secret (e.g. the FDE master) opens only on a non-tampered boot.
    pub fn seal_to_pcr(&self, master_key: &[u8; 32], pcr_digest: &[u8; 32], nonce: &[u8; 12]) -> Result<Vec<u8>, VaultError> {
        let k = derive_pcr_key(master_key, pcr_digest);
        self.seal(&k, nonce)
    }

    /// Unseal a PCR-bound blob with the master key and the CURRENT PCR measurement.
    /// If the PCRs do not match those at sealing time (changed boot), this
    /// fails with `Decrypt` — the secret remains inaccessible.
    pub fn unseal_from_pcr(blob: &[u8], master_key: &[u8; 32], current_pcr_digest: &[u8; 32]) -> Result<Vault, VaultError> {
        let k = derive_pcr_key(master_key, current_pcr_digest);
        Vault::unseal(blob, &k)
    }
}

/// KDF that binds the unseal key to (master, PCR state). Domain-separated
/// so the derived key cannot be (re)used for anything else.
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
        // With the correct cap → the value.
        assert_eq!(v.get("db-password", CAP_DB | CAP_OTHER).unwrap(), b"s3cr3t");
        // Without the cap → EPERM, even if you know the label.
        assert_eq!(v.get("db-password", CAP_OTHER), Err(VaultError::PermissionDenied));
        // Unknown label.
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
        // (list returns only labels + caps — the values do not come out)
    }

    #[test]
    fn seal_unseal_roundtrip() {
        let key = [0x33u8; 32];
        let nonce = [1u8; 12];
        let mut v = Vault::new();
        v.set("db-password", b"hunter2", CAP_DB);
        v.set("ssh-key", b"-----BEGIN-----", 0);
        let blob = v.seal(&key, &nonce).unwrap();
        // The blob contains NO plaintext.
        assert!(!blob.windows(7).any(|w| w == b"hunter2"));
        let v2 = Vault::unseal(&blob, &key).unwrap();
        assert_eq!(v2.get("db-password", CAP_DB).unwrap(), b"hunter2");
        assert_eq!(v2.get("ssh-key", 0).unwrap(), b"-----BEGIN-----");
    }

    #[test]
    fn pcr_seal_only_unseals_on_matching_boot_state() {
        // AF / PCR-seal: a vault bound to PCR state A opens ONLY under
        // those same PCRs. A changed boot (different PCR digest) → unsealing fails.
        let master = [0x77u8; 32];
        let nonce = [9u8; 12];
        let pcr_good = [0xAAu8; 32]; // measured-boot digest at seal time
        let pcr_tampered = [0xABu8; 32]; // one bit different = changed boot chain

        let mut v = Vault::new();
        v.set("fde-master", b"disk-key-material", CAP_DB);
        let blob = v.seal_to_pcr(&master, &pcr_good, &nonce).unwrap();

        // Same PCRs → unseals.
        let ok = Vault::unseal_from_pcr(&blob, &master, &pcr_good).unwrap();
        assert_eq!(ok.get("fde-master", CAP_DB).unwrap(), b"disk-key-material");
        // Tampered boot (different PCRs) → refused.
        assert_eq!(Vault::unseal_from_pcr(&blob, &master, &pcr_tampered).err(), Some(VaultError::Decrypt));
        // Correct PCRs but wrong master → also refused.
        assert_eq!(Vault::unseal_from_pcr(&blob, &[0u8; 32], &pcr_good).err(), Some(VaultError::Decrypt));
        // A blob that is PCR-bound does NOT open with the bare master (the binding is real).
        assert_eq!(Vault::unseal(&blob, &master).err(), Some(VaultError::Decrypt));
    }

    #[test]
    fn wrong_key_or_tamper_fails() {
        let key = [0x33u8; 32];
        let nonce = [2u8; 12];
        let mut v = Vault::new();
        v.set("x", b"geheim", 0);
        let mut blob = v.seal(&key, &nonce).unwrap();
        // Wrong key → Decrypt error.
        assert_eq!(Vault::unseal(&blob, &[0u8; 32]).err(), Some(VaultError::Decrypt));
        // Flip one byte (tamper) → Poly1305 detects it.
        let n = blob.len();
        blob[n - 1] ^= 0xFF;
        assert_eq!(Vault::unseal(&blob, &key).err(), Some(VaultError::Decrypt));
    }
}
