//! EuroIDM — soevereine bedrijfsidentiteit (plan V).
//!
//! EuroOS koppelt *identiteit* aan het *capability-model*: een gebruiker hoort bij
//! groepen, en groepen verlenen EuroGuard-capabilities. Een aanmelding levert een
//! **getekend token** (OIDC-achtig: subject + groepen + vervaltijd, Ed25519-getekend
//! door de IDM) dat diensten lokaal kunnen verifiëren zonder de IDM te bevragen — en
//! waaruit ze de effectieve capabilities afleiden. Geen verplichte externe identity
//! provider: de IDM draait lokaal (standalone of als brug naar LDAP/OIDC).
//!
//! Host-getest; `no_std`. Capabilities zijn een `u64`-bitset (subset van EuroGuard).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

const DOMAIN: &[u8] = b"EuroIDM-token-v1\0";

// ── Capabilities (subset van EuroGuard, voor groep→cap-mapping) ─────────────
pub const CAP_LOGIN: u64 = 1 << 0;
pub const CAP_NET: u64 = 1 << 1;
pub const CAP_FS_READ: u64 = 1 << 2;
pub const CAP_FS_WRITE: u64 = 1 << 3;
pub const CAP_AUDIT_READ: u64 = 1 << 4;
pub const CAP_USER_ADMIN: u64 = 1 << 5;
pub const CAP_IMMUTABLE_ADMIN: u64 = 1 << 6;
pub const CAP_SHUTDOWN: u64 = 1 << 7;

/// Een gebruiker: een uid, een naam en groepslidmaatschappen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub uid: u32,
    pub name: String,
    pub groups: Vec<String>,
}

/// De identiteitsopslag: gebruikers + de groep→capability-regels + de IDM-sleutel.
pub struct Idm {
    key: SigningKey,
    users: Vec<User>,
    group_caps: Vec<(String, u64)>,
}

/// Een getekend identiteitstoken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub subject: String,
    pub uid: u32,
    pub groups: Vec<String>,
    pub issued_at: u64,
    pub expires_at: u64,
    pub signature: [u8; 64],
}

/// Waarom een token afgewezen wordt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenError {
    BadSignature,
    Expired,
    NotYetValid,
    BadIssuerKey,
}

fn tbs(subject: &str, uid: u32, groups: &[String], iat: u64, exp: u64) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(DOMAIN);
    b.extend_from_slice(&(subject.len() as u32).to_le_bytes());
    b.extend_from_slice(subject.as_bytes());
    b.extend_from_slice(&uid.to_le_bytes());
    b.extend_from_slice(&(groups.len() as u32).to_le_bytes());
    for g in groups {
        b.extend_from_slice(&(g.len() as u32).to_le_bytes());
        b.extend_from_slice(g.as_bytes());
    }
    b.extend_from_slice(&iat.to_le_bytes());
    b.extend_from_slice(&exp.to_le_bytes());
    b
}

impl Idm {
    /// Maak een IDM met een 32-byte sleutel-seed.
    pub fn new(seed: [u8; 32]) -> Idm {
        Idm { key: SigningKey::from_bytes(&seed), users: Vec::new(), group_caps: Vec::new() }
    }

    /// De publieke sleutel waarmee diensten tokens verifiëren.
    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// Voeg een gebruiker toe.
    pub fn add_user(&mut self, uid: u32, name: &str, groups: &[&str]) {
        self.users.push(User {
            uid,
            name: String::from(name),
            groups: groups.iter().map(|g| String::from(*g)).collect(),
        });
    }

    /// Koppel een groep aan een capability-masker.
    pub fn set_group_caps(&mut self, group: &str, caps: u64) {
        if let Some(e) = self.group_caps.iter_mut().find(|(g, _)| g == group) {
            e.1 = caps;
        } else {
            self.group_caps.push((String::from(group), caps));
        }
    }

    pub fn lookup(&self, name: &str) -> Option<&User> {
        self.users.iter().find(|u| u.name == name)
    }

    /// De effectieve capabilities van een groepenlijst = de unie van de groep-maskers.
    pub fn caps_for_groups(&self, groups: &[String]) -> u64 {
        let mut caps = 0u64;
        for g in groups {
            if let Some((_, c)) = self.group_caps.iter().find(|(name, _)| name == g) {
                caps |= c;
            }
        }
        caps
    }

    /// Geef (na geslaagde aanmelding) een getekend token uit met geldigheid [`now`, `now+ttl`].
    pub fn issue_token(&self, name: &str, now: u64, ttl: u64) -> Option<Token> {
        let u = self.lookup(name)?;
        let exp = now + ttl;
        let signature = self.key.sign(&tbs(&u.name, u.uid, &u.groups, now, exp)).to_bytes();
        Some(Token {
            subject: u.name.clone(),
            uid: u.uid,
            groups: u.groups.clone(),
            issued_at: now,
            expires_at: exp,
            signature,
        })
    }
}

impl Token {
    /// Verifieer het token tegen de IDM-publieke sleutel op tijdstip `now`.
    pub fn verify(&self, idm_pubkey: &[u8; 32], now: u64) -> Result<(), TokenError> {
        let vk = VerifyingKey::from_bytes(idm_pubkey).map_err(|_| TokenError::BadIssuerKey)?;
        let sig = Signature::from_bytes(&self.signature);
        vk.verify(&tbs(&self.subject, self.uid, &self.groups, self.issued_at, self.expires_at), &sig)
            .map_err(|_| TokenError::BadSignature)?;
        if now < self.issued_at {
            return Err(TokenError::NotYetValid);
        }
        if now > self.expires_at {
            return Err(TokenError::Expired);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_000_000;

    fn idm() -> Idm {
        let mut idm = Idm::new([11u8; 32]);
        idm.set_group_caps("users", CAP_LOGIN | CAP_NET | CAP_FS_READ);
        idm.set_group_caps("admins", CAP_LOGIN | CAP_NET | CAP_FS_READ | CAP_FS_WRITE | CAP_USER_ADMIN | CAP_SHUTDOWN | CAP_IMMUTABLE_ADMIN);
        idm.set_group_caps("auditors", CAP_LOGIN | CAP_AUDIT_READ);
        idm.add_user(1000, "anke", &["users"]);
        idm.add_user(0, "root", &["admins"]);
        idm.add_user(1001, "controle", &["users", "auditors"]);
        idm
    }

    #[test]
    fn group_to_caps() {
        let idm = idm();
        let anke = idm.lookup("anke").unwrap();
        assert_eq!(idm.caps_for_groups(&anke.groups), CAP_LOGIN | CAP_NET | CAP_FS_READ);
        // Combinatie van twee groepen = unie.
        let c = idm.lookup("controle").unwrap();
        assert_eq!(idm.caps_for_groups(&c.groups), CAP_LOGIN | CAP_NET | CAP_FS_READ | CAP_AUDIT_READ);
        // Admin heeft user-admin, controle niet.
        assert_ne!(idm.caps_for_groups(&idm.lookup("root").unwrap().groups) & CAP_USER_ADMIN, 0);
    }

    #[test]
    fn token_roundtrip() {
        let idm = idm();
        let tok = idm.issue_token("anke", T0, 3600).unwrap();
        assert_eq!(tok.verify(&idm.public_key(), T0 + 100), Ok(()));
        assert_eq!(tok.uid, 1000);
        assert_eq!(idm.caps_for_groups(&tok.groups) & CAP_FS_WRITE, 0); // user mag niet schrijven
    }

    #[test]
    fn expired_token_rejected() {
        let idm = idm();
        let tok = idm.issue_token("anke", T0, 3600).unwrap();
        assert_eq!(tok.verify(&idm.public_key(), T0 + 7200), Err(TokenError::Expired));
    }

    #[test]
    fn tampered_token_rejected() {
        let idm = idm();
        let mut tok = idm.issue_token("anke", T0, 3600).unwrap();
        // Privilege-escalatie: voeg 'admins' toe na ondertekening.
        tok.groups.push(String::from("admins"));
        assert_eq!(tok.verify(&idm.public_key(), T0 + 100), Err(TokenError::BadSignature));
    }

    #[test]
    fn wrong_issuer_rejected() {
        let idm = idm();
        let tok = idm.issue_token("root", T0, 3600).unwrap();
        let other = Idm::new([99u8; 32]).public_key();
        assert_eq!(tok.verify(&other, T0 + 100), Err(TokenError::BadSignature));
    }

    #[test]
    fn unknown_user_no_token() {
        assert!(idm().issue_token("mallory", T0, 3600).is_none());
    }
}
