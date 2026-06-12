//! EuroID — soeverein gebruikersbeheer (Sprint K1 + P3 audit).
//!
//! De identiteits-autoriteit van EuroOS: gebruikers en groepen, een **Argon2id**-
//! gehashte credentialopslag, sessie-levenscyclus, per-gebruiker EuroGuard-
//! capabilities, een afdwingbaar wachtwoordbeleid, en een **hash-chain audit-log**
//! die elke gebruikersactie onomkeerbaar vastlegt (NIS2 Art. 21, GDPR Art. 5(2)/32,
//! ISO 27001 A.9).
//!
//! De kern is `no_std` + host-testbaar en bevat geen kloktoegang of RNG: tijd
//! (`Timestamp`) en zout (salt) worden er door de aanroeper ingebracht (de kernel
//! levert TPM-RNG en de tick-klok). Zo blijft alles deterministisch testbaar.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod argon2;
pub mod audit;
pub mod auth;
pub mod cred;
pub mod model;
pub mod persist;
pub mod policy;

pub use audit::{AuditEntry, AuditEvent, AuditLog, DenyReason};
pub use auth::{authenticate, AuthError, Credential, Session};
pub use cred::{Argon2idHash, PasswordRecord, ARGON2_M_COST, ARGON2_P_COST, ARGON2_T_COST, SALT_LEN};
pub use model::{
    effective_caps, Group, GroupDb, LockReason, User, UserDb, UserError, UserState,
};
pub use policy::{validate_password, validate_username, PasswordPolicy, PolicyError};

use alloc::string::String;

// ─────────────────────────────────────────────────────────────────────────────
// Newtypes — een uid/gid is NOOIT een kale u32.
// ─────────────────────────────────────────────────────────────────────────────

/// Een gebruikers-ID. Onveranderlijk na aanmaak.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct UserId(pub u32);

/// Een groeps-ID.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct GroupId(pub u32);

impl UserId {
    /// Het systeem-/root-subject (uid 0). Gebruikt voor systeemgeïnitieerde acties
    /// (bv. automatische account-vergrendeling).
    pub const SYSTEM: UserId = UserId(0);
    pub const ROOT: UserId = UserId(0);

    /// Eerste uid voor gewone (niet-systeem) gebruikers.
    pub const FIRST_REGULAR: u32 = 1000;
    /// Eerste uid voor systeem-/service-accounts.
    pub const FIRST_SYSTEM: u32 = 100;
}

// Ingebouwde groepen (aangemaakt bij systeem-init, kunnen niet verwijderd worden).
pub const GROUP_WHEEL: GroupId = GroupId(0); // Volledig admin — CAP_ALL
pub const GROUP_AUDIT: GroupId = GroupId(1); // Mag het audit-log lezen
pub const GROUP_NET: GroupId = GroupId(2); // Netwerktoegang
pub const GROUP_VAULT: GroupId = GroupId(3); // EuroVault-toegang
pub const GROUP_AGENT: GroupId = GroupId(4); // EuroAgent starten
pub const GROUP_USERS: GroupId = GroupId(100); // Standaard voor nieuwe gebruikers

// ─────────────────────────────────────────────────────────────────────────────
// Capabilities — bitset (subset/superset van EuroGuard, voor identiteit→cap).
// ─────────────────────────────────────────────────────────────────────────────

/// Een EuroGuard-capabilityset als bitmasker.
pub type Caps = u64;

pub const CAP_LOGIN: Caps = 1 << 0;
pub const CAP_FILE_READ: Caps = 1 << 1;
pub const CAP_FILE_WRITE: Caps = 1 << 2;
pub const CAP_FILE: Caps = CAP_FILE_READ | CAP_FILE_WRITE;
pub const CAP_NET: Caps = 1 << 3;
pub const CAP_DISPLAY: Caps = 1 << 4;
pub const CAP_AUDIO: Caps = 1 << 5;
pub const CAP_VAULT_READ: Caps = 1 << 6;
pub const CAP_VAULT_WRITE: Caps = 1 << 7;
pub const CAP_VAULT: Caps = CAP_VAULT_READ | CAP_VAULT_WRITE;
pub const CAP_AGENT_SPAWN: Caps = 1 << 8;
pub const CAP_AUDIT_READ: Caps = 1 << 9;
pub const CAP_USER_ADMIN: Caps = 1 << 10;
pub const CAP_IMMUTABLE_ADMIN: Caps = 1 << 11;
pub const CAP_SHUTDOWN: Caps = 1 << 12;
/// Alle capabilities (wheel-groep).
pub const CAP_ALL: Caps = !0;

/// Een door beleid toegestaan masker dat niets weigert (default EuroPol-staat).
pub const ALLOW_ALL: Caps = !0;

const CAP_NAMES: &[(&str, Caps)] = &[
    ("CAP_LOGIN", CAP_LOGIN),
    ("CAP_FILE", CAP_FILE),
    ("CAP_FILE_READ", CAP_FILE_READ),
    ("CAP_FILE_WRITE", CAP_FILE_WRITE),
    ("CAP_NET", CAP_NET),
    ("CAP_DISPLAY", CAP_DISPLAY),
    ("CAP_AUDIO", CAP_AUDIO),
    ("CAP_VAULT", CAP_VAULT),
    ("CAP_VAULT_READ", CAP_VAULT_READ),
    ("CAP_VAULT_WRITE", CAP_VAULT_WRITE),
    ("CAP_AGENT_SPAWN", CAP_AGENT_SPAWN),
    ("CAP_AUDIT_READ", CAP_AUDIT_READ),
    ("CAP_USER_ADMIN", CAP_USER_ADMIN),
    ("CAP_IMMUTABLE_ADMIN", CAP_IMMUTABLE_ADMIN),
    ("CAP_SHUTDOWN", CAP_SHUTDOWN),
];

/// Zet een capability-naam (bv. `"CAP_NET"`) om naar zijn bit. `CAP_ALL` → alles.
pub fn cap_from_name(name: &str) -> Option<Caps> {
    let n = name.trim();
    if n.eq_ignore_ascii_case("CAP_ALL") {
        return Some(CAP_ALL);
    }
    CAP_NAMES
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(n))
        .map(|(_, v)| *v)
}

/// Een leesbare opsomming van de gezette capabilities (samenvatting voor audit/`id`).
pub fn cap_names(caps: Caps) -> alloc::vec::Vec<String> {
    use alloc::string::ToString;
    if caps == CAP_ALL {
        return alloc::vec!["CAP_ALL".to_string()];
    }
    let mut out = alloc::vec::Vec::new();
    // De samengestelde namen eerst (CAP_FILE, CAP_VAULT) als beide bits gezet zijn.
    let mut shown: Caps = 0;
    for (name, bit) in [("CAP_FILE", CAP_FILE), ("CAP_VAULT", CAP_VAULT)] {
        if caps & bit == bit {
            out.push(name.to_string());
            shown |= bit;
        }
    }
    for (name, bit) in CAP_NAMES {
        if *bit == CAP_FILE || *bit == CAP_VAULT {
            continue;
        }
        if caps & bit != 0 && shown & bit == 0 {
            out.push(name.to_string());
            shown |= bit;
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Tijd — een eenvoudige seconden-sinds-epoch (door de aanroeper ingebracht).
// ─────────────────────────────────────────────────────────────────────────────

/// Een tijdstempel in seconden (de kernel levert wandkloktijd; tests een vaste waarde).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub fn secs(self) -> u64 {
        self.0
    }
}

/// Constant-time vergelijking van twee byte-slices (tegen timing-zijkanalen).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Hex-codering (kleine letters) voor hashes/sessie-ID's in het audit-log.
pub fn hex(bytes: &[u8]) -> String {
    use alloc::string::ToString;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    if s.is_empty() {
        "".to_string()
    } else {
        s
    }
}
