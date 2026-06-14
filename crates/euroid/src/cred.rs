//! Credential storage: Argon2id-hashed passwords, with history (no reuse).

use alloc::string::String;
use alloc::vec::Vec;

use crate::argon2::{self, Params};
use crate::{ct_eq, hex, Timestamp};

/// Argon2id parameters — sovereign defaults, never negotiated down.
pub const ARGON2_M_COST: u32 = 65536; // 64 MiB memory
pub const ARGON2_T_COST: u32 = 3; // 3 iterations
pub const ARGON2_P_COST: u32 = 4; // 4 parallel lanes
pub const SALT_LEN: usize = 32; // 256-bit salt (TPM-RNG)
const TAG_LEN: usize = 32;

/// The sovereign default parameters.
pub fn default_params() -> Params {
    Params { m_cost: ARGON2_M_COST, t_cost: ARGON2_T_COST, p_cost: ARGON2_P_COST, tag_len: TAG_LEN }
}

/// A single Argon2id hash with its salt and parameters (self-describing, PHC-like).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Argon2idHash {
    pub salt: Vec<u8>,
    pub tag: Vec<u8>,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Argon2idHash {
    /// Hash a password with the given parameters and the (caller-supplied,
    /// preferably TPM-RNG) salt.
    pub fn create(password: &[u8], salt: &[u8], params: &Params) -> Argon2idHash {
        let tag = argon2::argon2id(password, salt, &[], &[], params);
        Argon2idHash {
            salt: salt.to_vec(),
            tag,
            m_cost: params.m_cost,
            t_cost: params.t_cost,
            p_cost: params.p_cost,
        }
    }

    /// Hash with the sovereign default parameters.
    pub fn create_default(password: &[u8], salt: &[u8]) -> Argon2idHash {
        Argon2idHash::create(password, salt, &default_params())
    }

    /// Verify a password against this hash — recomputes the tag with the same
    /// salt and the same parameters and compares constant-time.
    pub fn verify(&self, password: &[u8]) -> bool {
        let params = Params {
            m_cost: self.m_cost,
            t_cost: self.t_cost,
            p_cost: self.p_cost,
            tag_len: self.tag.len(),
        };
        let got = argon2::argon2id(password, &self.salt, &[], &[], &params);
        ct_eq(&got, &self.tag)
    }

    /// PHC-like encoding, e.g. `$argon2id$m=65536,t=3,p=4$<salt-hex>$<tag-hex>`.
    pub fn encode(&self) -> String {
        alloc::format!(
            "$argon2id$m={},t={},p={}${}${}",
            self.m_cost,
            self.t_cost,
            self.p_cost,
            hex(&self.salt),
            hex(&self.tag)
        )
    }
}

/// A user's password record (in `shadow.db`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasswordRecord {
    pub hash: Argon2idHash,
    pub changed_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub must_change: bool,
    /// The last N hashes (to prevent reuse).
    pub history: Vec<Argon2idHash>,
    /// An account without a usable password (locked until one is set).
    pub locked: bool,
}

impl PasswordRecord {
    /// Create a record by hashing a password.
    pub fn hash_password(password: &[u8], salt: &[u8], params: &Params, now: Timestamp) -> PasswordRecord {
        PasswordRecord {
            hash: Argon2idHash::create(password, salt, params),
            changed_at: now,
            expires_at: None,
            must_change: false,
            history: Vec::new(),
            locked: false,
        }
    }

    /// An account without a password — locked until one is set.
    pub fn locked() -> PasswordRecord {
        PasswordRecord {
            hash: Argon2idHash { salt: Vec::new(), tag: Vec::new(), m_cost: 0, t_cost: 0, p_cost: 0 },
            changed_at: Timestamp::default(),
            expires_at: None,
            must_change: true,
            history: Vec::new(),
            locked: true,
        }
    }

    /// Verify a password (false if the account is locked / has no hash).
    pub fn verify(&self, password: &[u8]) -> bool {
        if self.locked || self.hash.tag.is_empty() {
            return false;
        }
        self.hash.verify(password)
    }

    /// Is this password equal to the current or one of the last `depth` hashes?
    pub fn is_reused(&self, password: &[u8], depth: usize) -> bool {
        if !self.hash.tag.is_empty() && self.hash.verify(password) {
            return true;
        }
        for old in self.history.iter().take(depth) {
            if old.verify(password) {
                return true;
            }
        }
        false
    }

    /// Replace the hash; keep the old one in the history (bounded by `history_depth`).
    pub fn set_new(&mut self, new_hash: Argon2idHash, history_depth: usize, now: Timestamp) {
        if !self.hash.tag.is_empty() {
            self.history.insert(0, self.hash.clone());
        }
        if self.history.len() > history_depth {
            self.history.truncate(history_depth);
        }
        self.hash = new_hash;
        self.changed_at = now;
        self.must_change = false;
        self.locked = false;
    }

    /// Is the password expired at time `now`?
    pub fn is_expired(&self, now: Timestamp) -> bool {
        matches!(self.expires_at, Some(e) if now > e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fast test parameters (the RFC correctness is tested elsewhere with the real params).
    fn fast() -> Params {
        Params { m_cost: 256, t_cost: 2, p_cost: 1, tag_len: 32 }
    }

    #[test]
    fn hash_verify_roundtrip() {
        let salt = [7u8; SALT_LEN];
        let rec = PasswordRecord::hash_password(b"Correct-Horse-9!", &salt, &fast(), Timestamp(100));
        assert!(rec.verify(b"Correct-Horse-9!"));
        assert!(!rec.verify(b"wrong-password"));
    }

    #[test]
    fn locked_record_never_verifies() {
        let rec = PasswordRecord::locked();
        assert!(!rec.verify(b"anything"));
        assert!(rec.locked);
    }

    #[test]
    fn history_blocks_reuse() {
        let salt = [1u8; SALT_LEN];
        let mut rec = PasswordRecord::hash_password(b"Pw-one-111!", &salt, &fast(), Timestamp(1));
        // Rotate through 12 passwords.
        for n in 2..=12u32 {
            let pw = alloc::format!("Pw-num-{n:03}!");
            let salt2 = [n as u8; SALT_LEN];
            let h = Argon2idHash::create(pw.as_bytes(), &salt2, &fast());
            rec.set_new(h, 12, Timestamp(n as u64));
        }
        // The very first password is still in the history → reuse rejected.
        assert!(rec.is_reused(b"Pw-one-111!", 12));
        // A fresh password is not reused.
        assert!(!rec.is_reused(b"Brand-New-42!", 12));
    }

    #[test]
    fn encoding_is_self_describing() {
        let salt = [9u8; SALT_LEN];
        let h = Argon2idHash::create(b"x", &salt, &fast());
        let enc = h.encode();
        assert!(enc.starts_with("$argon2id$m=256,t=2,p=1$"));
    }
}
