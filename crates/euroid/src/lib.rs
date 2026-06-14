//! EuroID — sovereign user management (Sprint K1 + P3 audit).
//!
//! The identity authority of EuroOS: users and groups, an **Argon2id**-hashed
//! credential store, session lifecycle, per-user EuroGuard capabilities, an
//! enforceable password policy, and a **hash-chain audit log** that records every
//! user action irreversibly (NIS2 Art. 21, GDPR Art. 5(2)/32, ISO 27001 A.9).
//!
//! The core is `no_std` + host-testable and contains no clock access or RNG: time
//! (`Timestamp`) and salt are injected by the caller (the kernel provides the
//! TPM-RNG and the tick clock). This keeps everything deterministically testable.

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
// Newtypes — a uid/gid is NEVER a bare u32.
// ─────────────────────────────────────────────────────────────────────────────

/// A user ID. Immutable after creation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct UserId(pub u32);

/// A group ID.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct GroupId(pub u32);

impl UserId {
    /// The system/root subject (uid 0). Used for system-initiated actions
    /// (e.g. automatic account locking).
    pub const SYSTEM: UserId = UserId(0);
    pub const ROOT: UserId = UserId(0);

    /// First uid for regular (non-system) users.
    pub const FIRST_REGULAR: u32 = 1000;
    /// First uid for system/service accounts.
    pub const FIRST_SYSTEM: u32 = 100;
}

// Built-in groups (created at system init, cannot be deleted).
pub const GROUP_WHEEL: GroupId = GroupId(0); // Full admin — CAP_ALL
pub const GROUP_AUDIT: GroupId = GroupId(1); // May read the audit log
pub const GROUP_NET: GroupId = GroupId(2); // Network access
pub const GROUP_VAULT: GroupId = GroupId(3); // EuroVault access
pub const GROUP_AGENT: GroupId = GroupId(4); // Start EuroAgent
pub const GROUP_USERS: GroupId = GroupId(100); // Default for new users

// ─────────────────────────────────────────────────────────────────────────────
// Capabilities — bitset (subset/superset of EuroGuard, for identity→cap).
// ─────────────────────────────────────────────────────────────────────────────

/// An EuroGuard capability set as a bitmask.
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
/// All capabilities (wheel group).
pub const CAP_ALL: Caps = !0;

/// A policy-allowed mask that denies nothing (default EuroPol state).
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

/// Convert a capability name (e.g. `"CAP_NET"`) to its bit. `CAP_ALL` → everything.
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

/// A readable listing of the set capabilities (summary for audit/`id`).
pub fn cap_names(caps: Caps) -> alloc::vec::Vec<String> {
    use alloc::string::ToString;
    if caps == CAP_ALL {
        return alloc::vec!["CAP_ALL".to_string()];
    }
    let mut out = alloc::vec::Vec::new();
    // The composite names first (CAP_FILE, CAP_VAULT) if both bits are set.
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
// Time — a simple seconds-since-epoch (injected by the caller).
// ─────────────────────────────────────────────────────────────────────────────

/// A timestamp in seconds (the kernel provides wall-clock time; tests a fixed value).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub fn secs(self) -> u64 {
        self.0
    }
}

/// Constant-time comparison of two byte slices (against timing side channels).
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

/// Hex encoding (lowercase) for hashes/session IDs in the audit log.
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
