//! Credential-opslag: Argon2id-gehashte wachtwoorden, met geschiedenis (geen hergebruik).

use alloc::string::String;
use alloc::vec::Vec;

use crate::argon2::{self, Params};
use crate::{ct_eq, hex, Timestamp};

/// Argon2id-parameters — soevereine defaults, nooit omlaag onderhandeld.
pub const ARGON2_M_COST: u32 = 65536; // 64 MiB geheugen
pub const ARGON2_T_COST: u32 = 3; // 3 iteraties
pub const ARGON2_P_COST: u32 = 4; // 4 parallelle lanes
pub const SALT_LEN: usize = 32; // 256-bit zout (TPM-RNG)
const TAG_LEN: usize = 32;

/// De soevereine standaardparameters.
pub fn default_params() -> Params {
    Params { m_cost: ARGON2_M_COST, t_cost: ARGON2_T_COST, p_cost: ARGON2_P_COST, tag_len: TAG_LEN }
}

/// Eén Argon2id-hash met zijn zout en parameters (zelf-beschrijvend, PHC-achtig).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Argon2idHash {
    pub salt: Vec<u8>,
    pub tag: Vec<u8>,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Argon2idHash {
    /// Hash een wachtwoord met de gegeven parameters en het (door de aanroeper
    /// geleverde, bij voorkeur TPM-RNG) zout.
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

    /// Hash met de soevereine standaardparameters.
    pub fn create_default(password: &[u8], salt: &[u8]) -> Argon2idHash {
        Argon2idHash::create(password, salt, &default_params())
    }

    /// Verifieer een wachtwoord tegen deze hash — herberekent de tag met hetzelfde
    /// zout en dezelfde parameters en vergelijkt constant-time.
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

    /// PHC-achtige codering, bv. `$argon2id$m=65536,t=3,p=4$<salt-hex>$<tag-hex>`.
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

/// Het wachtwoord-record van een gebruiker (in `shadow.db`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasswordRecord {
    pub hash: Argon2idHash,
    pub changed_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub must_change: bool,
    /// De laatste N hashes (om hergebruik te voorkomen).
    pub history: Vec<Argon2idHash>,
    /// Een account zónder bruikbaar wachtwoord (vergrendeld tot er één gezet wordt).
    pub locked: bool,
}

impl PasswordRecord {
    /// Maak een record door een wachtwoord te hashen.
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

    /// Een account zonder wachtwoord — vergrendeld tot er één gezet wordt.
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

    /// Verifieer een wachtwoord (false als het account vergrendeld is / geen hash heeft).
    pub fn verify(&self, password: &[u8]) -> bool {
        if self.locked || self.hash.tag.is_empty() {
            return false;
        }
        self.hash.verify(password)
    }

    /// Is dit wachtwoord gelijk aan de huidige of een van de laatste `depth` hashes?
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

    /// Vervang de hash; bewaar de oude in de geschiedenis (begrensd op `history_depth`).
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

    /// Is het wachtwoord verlopen op tijdstip `now`?
    pub fn is_expired(&self, now: Timestamp) -> bool {
        matches!(self.expires_at, Some(e) if now > e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Snelle testparameters (de RFC-correctheid is elders met de echte params getest).
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
        // Roteer door 12 wachtwoorden.
        for n in 2..=12u32 {
            let pw = alloc::format!("Pw-num-{n:03}!");
            let salt2 = [n as u8; SALT_LEN];
            let h = Argon2idHash::create(pw.as_bytes(), &salt2, &fast());
            rec.set_new(h, 12, Timestamp(n as u64));
        }
        // Het allereerste wachtwoord zit nog in de geschiedenis → hergebruik geweigerd.
        assert!(rec.is_reused(b"Pw-one-111!", 12));
        // Een vers wachtwoord is niet hergebruikt.
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
