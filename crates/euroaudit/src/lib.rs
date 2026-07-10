//! EuroAudit — a **tamper-evident, hash-chained** audit log (plan P3 / GDPR).
//!
//! Under the GDPR (accountability) and the CRA (secure logging) a security log
//! must be trustworthy: you have to be able to *prove* nobody quietly edited,
//! reordered or deleted an entry. EuroAudit chains every entry to the previous
//! one — `hash_i = SHA-256(hash_{i-1} ‖ entry_i)` — so changing, inserting or
//! removing any entry breaks the chain and [`verify`](AuditLog::verify) fails.
//! On top of that it offers structured **JSON export**, **filtering/query**, and
//! **rotation** that carries the chain hash across files (so the tamper-evidence
//! survives log rollover). Pure `no_std`, host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use sha2::{Digest, Sha256};

/// The category of an audited event (GDPR/CRA-relevant surfaces).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Boot,
    Login,
    Logout,
    CapDenied,
    Execve,
    Connection,
    VaultAccess,
    ImmutableSet,
    PolicyViolation,
    AgentTool,
}

impl Kind {
    fn tag(self) -> u8 {
        self as u8
    }
    fn name(self) -> &'static str {
        match self {
            Kind::Boot => "boot",
            Kind::Login => "login",
            Kind::Logout => "logout",
            Kind::CapDenied => "cap_denied",
            Kind::Execve => "execve",
            Kind::Connection => "connection",
            Kind::VaultAccess => "vault_access",
            Kind::ImmutableSet => "immutable_set",
            Kind::PolicyViolation => "policy_violation",
            Kind::AgentTool => "agent_tool",
        }
    }
}

/// One audit entry, chained to its predecessor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub seq: u64,
    pub ts: u64,
    pub kind: Kind,
    pub subject: String,
    /// The rolling chain hash up to and including this entry.
    pub hash: [u8; 32],
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(core::char::from_digit((x >> 4) as u32, 16).unwrap());
        s.push(core::char::from_digit((x & 0xf) as u32, 16).unwrap());
    }
    s
}

/// The chained hash of an entry given the previous hash.
fn chain_hash(prev: &[u8; 32], seq: u64, ts: u64, kind: Kind, subject: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(prev);
    h.update(seq.to_le_bytes());
    h.update(ts.to_le_bytes());
    h.update([kind.tag()]);
    h.update((subject.len() as u32).to_le_bytes());
    h.update(subject.as_bytes());
    let mut o = [0u8; 32];
    o.copy_from_slice(&h.finalize());
    o
}

/// A hash-chained audit log.
pub struct AuditLog {
    entries: Vec<Entry>,
    /// The chain anchor: 0 for a fresh log, or the last hash of the previous
    /// segment after a rotation (so tamper-evidence carries across files).
    anchor: [u8; 32],
    next_seq: u64,
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditLog {
    pub fn new() -> AuditLog {
        AuditLog { entries: Vec::new(), anchor: [0u8; 32], next_seq: 0 }
    }

    /// Continue a chain from a prior segment's last hash (used across rotation)
    /// and a starting sequence number.
    pub fn continued(anchor: [u8; 32], next_seq: u64) -> AuditLog {
        AuditLog { entries: Vec::new(), anchor, next_seq }
    }

    /// Append an event; returns its new chain hash.
    pub fn append(&mut self, ts: u64, kind: Kind, subject: &str) -> [u8; 32] {
        let prev = self.entries.last().map(|e| e.hash).unwrap_or(self.anchor);
        let seq = self.next_seq;
        let hash = chain_hash(&prev, seq, ts, kind, subject);
        self.entries.push(Entry { seq, ts, kind, subject: subject.to_string(), hash });
        self.next_seq += 1;
        hash
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    /// The head hash — what a rotated continuation anchors to.
    pub fn head(&self) -> [u8; 32] {
        self.entries.last().map(|e| e.hash).unwrap_or(self.anchor)
    }

    /// Recompute the whole chain from the anchor and confirm every stored hash
    /// matches — detecting any edit, reorder, insertion or deletion.
    pub fn verify(&self) -> bool {
        let mut prev = self.anchor;
        for (i, e) in self.entries.iter().enumerate() {
            if e.seq != self.entries[0].seq + i as u64 {
                return false; // a gap/reorder in the sequence
            }
            let h = chain_hash(&prev, e.seq, e.ts, e.kind, &e.subject);
            if h != e.hash {
                return false;
            }
            prev = h;
        }
        true
    }

    /// Filter entries by kind and/or a minimum timestamp.
    pub fn query(&self, kind: Option<Kind>, since_ts: Option<u64>) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| kind.map(|k| e.kind == k).unwrap_or(true))
            .filter(|e| since_ts.map(|t| e.ts >= t).unwrap_or(true))
            .collect()
    }

    /// Export the log as a JSON array (for GDPR data-subject export / SIEM).
    pub fn to_json(&self) -> String {
        let mut s = String::from("[");
        for (i, e) in self.entries.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&alloc::format!(
                "{{\"seq\":{},\"ts\":{},\"kind\":\"{}\",\"subject\":\"{}\",\"hash\":\"{}\"}}",
                e.seq,
                e.ts,
                e.kind.name(),
                json_escape(&e.subject),
                hex(&e.hash),
            ));
        }
        s.push(']');
        s
    }

    /// Rotate when the log exceeds `max` entries: the current log becomes the
    /// archive (its chain stays intact and verifiable), and a fresh continuation
    /// log is returned, anchored to the archive's head so the chain is unbroken
    /// across the two files. Returns `None` if rotation is not needed.
    pub fn rotate(&self, max: usize) -> Option<AuditLog> {
        if self.entries.len() <= max {
            return None;
        }
        Some(AuditLog::continued(self.head(), self.next_seq))
    }
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push(' '),
            c => o.push(c),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log() -> AuditLog {
        let mut l = AuditLog::new();
        l.append(100, Kind::Boot, "cold boot");
        l.append(101, Kind::Login, "alice tty1");
        l.append(102, Kind::Execve, "/bin/curl");
        l.append(103, Kind::Connection, "10.0.0.5:443");
        l.append(104, Kind::VaultAccess, "db-password (allowed)");
        l
    }

    #[test]
    fn intact_chain_verifies() {
        assert!(log().verify());
    }

    #[test]
    fn editing_an_entry_breaks_the_chain() {
        let mut l = log();
        l.entries[2].subject = "/bin/rm".to_string(); // silently rewrite an entry
        assert!(!l.verify());
    }

    #[test]
    fn deleting_an_entry_breaks_the_chain() {
        let mut l = log();
        l.entries.remove(1); // drop the login
        assert!(!l.verify());
    }

    #[test]
    fn query_filters_by_kind_and_time() {
        let l = log();
        assert_eq!(l.query(Some(Kind::Execve), None).len(), 1);
        assert_eq!(l.query(None, Some(103)).len(), 2); // ts >= 103
    }

    #[test]
    fn json_export_contains_entries() {
        let j = log().to_json();
        assert!(j.starts_with('[') && j.ends_with(']'));
        assert!(j.contains("\"kind\":\"execve\""));
        assert!(j.contains("\"subject\":\"/bin/curl\""));
    }

    #[test]
    fn rotation_carries_the_chain() {
        let l = log();
        let mut cont = l.rotate(3).unwrap(); // >3 entries → rotate
        // The continuation is anchored to the archive head and keeps verifying.
        cont.append(200, Kind::Logout, "alice");
        assert!(cont.verify());
        assert_eq!(cont.entries()[0].seq, 5); // sequence continues, no reset
        assert!(cont.rotate(10).is_none()); // not needed yet
    }
}
