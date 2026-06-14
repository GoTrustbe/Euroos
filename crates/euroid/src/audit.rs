//! Append-only **hash-chain audit log** (P3 + GDPR Art. 5(2)/32, NIS2 Art. 21).
//!
//! Every user action is recorded as a self-describing JSON record. Each record
//! contains the hash of the previous record (`prev_hash`) plus its own `hash`
//! over `seq ‖ prev_hash ‖ body`. As a result, any change to an older record
//! invalidates all following hashes — the log is tamper-evident, even without
//! the EuroFS APPEND_ONLY flag (which additionally makes it physically irreversible).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use sha2::{Digest, Sha256};

use crate::model::LockReason;
use crate::{cap_names, hex, Caps, GroupId, Timestamp, UserId};

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&alloc::format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_uid_array(uids: &[GroupId]) -> String {
    let mut s = String::from("[");
    for (i, g) in uids.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&g.0.to_string());
    }
    s.push(']');
    s
}

fn json_caps(caps: Caps) -> String {
    let names = cap_names(caps);
    let mut s = String::from("[");
    for (i, n) in names.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&json_str(n));
    }
    s.push(']');
    s
}

/// Why a login was denied (without revealing whether the user exists).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DenyReason {
    UnknownUser,
    AccountLocked(LockReason),
    AccountExpired,
    AccountDeleted,
}

impl DenyReason {
    fn tag(&self) -> String {
        match self {
            DenyReason::UnknownUser => "unknown-user".to_string(),
            DenyReason::AccountLocked(r) => alloc::format!("account-locked:{}", r.tag()),
            DenyReason::AccountExpired => "account-expired".to_string(),
            DenyReason::AccountDeleted => "account-deleted".to_string(),
        }
    }
}

/// Each audit event. Serialized as JSON to the append-only log.
#[derive(Clone, Debug)]
pub enum AuditEvent {
    Boot,
    SystemInit,
    UserCreated { uid: UserId, username: String, created_by: UserId, groups: Vec<GroupId>, caps: Caps },
    UserModified { uid: UserId, username: String, modified_by: UserId, change: String },
    UserDeleted { uid: UserId, username: String, deleted_by: UserId },
    UserLocked { uid: UserId, username: String, reason: LockReason, locked_by: UserId },
    UserUnlocked { uid: UserId, username: String, unlocked_by: UserId },
    LoginSuccess { uid: UserId, username: String, session: String, caps: Caps, tty: String },
    LoginFailed { uid: UserId, username: String, attempt: u32 },
    LoginDenied { username: String, reason: DenyReason },
    Logout { uid: UserId, username: String, session: String, duration_secs: u64 },
    PasswordChanged { actor: UserId, target: UserId, admin_reset: bool },
    PasswordChangeFailed { actor: UserId, target: UserId, reason: String },
    SudoUsed { uid: UserId, username: String, command: String, success: bool },
    SuSwitched { from_uid: UserId, to_uid: UserId, session: String },
}

impl AuditEvent {
    /// The event name (`"event"` field).
    pub fn name(&self) -> &'static str {
        match self {
            AuditEvent::Boot => "Boot",
            AuditEvent::SystemInit => "SystemInit",
            AuditEvent::UserCreated { .. } => "UserCreated",
            AuditEvent::UserModified { .. } => "UserModified",
            AuditEvent::UserDeleted { .. } => "UserDeleted",
            AuditEvent::UserLocked { .. } => "UserLocked",
            AuditEvent::UserUnlocked { .. } => "UserUnlocked",
            AuditEvent::LoginSuccess { .. } => "LoginSuccess",
            AuditEvent::LoginFailed { .. } => "LoginFailed",
            AuditEvent::LoginDenied { .. } => "LoginDenied",
            AuditEvent::Logout { .. } => "Logout",
            AuditEvent::PasswordChanged { .. } => "PasswordChanged",
            AuditEvent::PasswordChangeFailed { .. } => "PasswordChangeFailed",
            AuditEvent::SudoUsed { .. } => "SudoUsed",
            AuditEvent::SuSwitched { .. } => "SuSwitched",
        }
    }

    /// The event-specific fields as a JSON fragment (without braces).
    /// GDPR Art. 32 pseudonymization: we log UIDs, not names-as-key.
    fn fields_json(&self) -> String {
        match self {
            AuditEvent::Boot | AuditEvent::SystemInit => String::new(),
            AuditEvent::UserCreated { uid, username, created_by, groups, caps } => alloc::format!(
                ",\"uid\":{},\"username\":{},\"created_by\":{},\"groups\":{},\"caps\":{}",
                uid.0,
                json_str(username),
                created_by.0,
                json_uid_array(groups),
                json_caps(*caps)
            ),
            AuditEvent::UserModified { uid, username, modified_by, change } => alloc::format!(
                ",\"uid\":{},\"username\":{},\"modified_by\":{},\"change\":{}",
                uid.0,
                json_str(username),
                modified_by.0,
                json_str(change)
            ),
            AuditEvent::UserDeleted { uid, username, deleted_by } => alloc::format!(
                ",\"uid\":{},\"username\":{},\"deleted_by\":{}",
                uid.0,
                json_str(username),
                deleted_by.0
            ),
            AuditEvent::UserLocked { uid, username, reason, locked_by } => alloc::format!(
                ",\"uid\":{},\"username\":{},\"reason\":{},\"locked_by\":{}",
                uid.0,
                json_str(username),
                json_str(reason.tag()),
                locked_by.0
            ),
            AuditEvent::UserUnlocked { uid, username, unlocked_by } => alloc::format!(
                ",\"uid\":{},\"username\":{},\"unlocked_by\":{}",
                uid.0,
                json_str(username),
                unlocked_by.0
            ),
            AuditEvent::LoginSuccess { uid, username, session, caps, tty } => alloc::format!(
                ",\"uid\":{},\"username\":{},\"session\":{},\"caps_summary\":{},\"tty\":{}",
                uid.0,
                json_str(username),
                json_str(session),
                json_caps(*caps),
                json_str(tty)
            ),
            AuditEvent::LoginFailed { uid, username, attempt } => alloc::format!(
                ",\"uid\":{},\"username\":{},\"attempt_count\":{}",
                uid.0,
                json_str(username),
                attempt
            ),
            AuditEvent::LoginDenied { username, reason } => alloc::format!(
                ",\"username\":{},\"reason\":{}",
                json_str(username),
                json_str(&reason.tag())
            ),
            AuditEvent::Logout { uid, username, session, duration_secs } => alloc::format!(
                ",\"uid\":{},\"username\":{},\"session\":{},\"duration_secs\":{}",
                uid.0,
                json_str(username),
                json_str(session),
                duration_secs
            ),
            AuditEvent::PasswordChanged { actor, target, admin_reset } => alloc::format!(
                ",\"actor\":{},\"target\":{},\"admin_reset\":{}",
                actor.0,
                target.0,
                admin_reset
            ),
            AuditEvent::PasswordChangeFailed { actor, target, reason } => alloc::format!(
                ",\"actor\":{},\"target\":{},\"reason\":{}",
                actor.0,
                target.0,
                json_str(reason)
            ),
            AuditEvent::SudoUsed { uid, username, command, success } => alloc::format!(
                ",\"uid\":{},\"username\":{},\"command\":{},\"success\":{}",
                uid.0,
                json_str(username),
                json_str(command),
                success
            ),
            AuditEvent::SuSwitched { from_uid, to_uid, session } => alloc::format!(
                ",\"from_uid\":{},\"to_uid\":{},\"session\":{}",
                from_uid.0,
                to_uid.0,
                json_str(session)
            ),
        }
    }
}

/// One record in the chain.
#[derive(Clone, Debug)]
pub struct AuditEntry {
    pub seq: u64,
    pub event: String,
    pub timestamp: Timestamp,
    /// The serialized body (`seq` + `event` + fields + `timestamp`) that is hashed over.
    pub body: String,
    pub prev_hash: [u8; 32],
    pub hash: [u8; 32],
}

impl AuditEntry {
    fn compute_hash(seq: u64, prev_hash: &[u8; 32], body: &str) -> [u8; 32] {
        let mut buf = Vec::with_capacity(8 + 32 + body.len());
        buf.extend_from_slice(&seq.to_le_bytes());
        buf.extend_from_slice(prev_hash);
        buf.extend_from_slice(body.as_bytes());
        sha256(&buf)
    }

    /// The full JSON line as stored on disk (incl. prev_hash/hash).
    pub fn line(&self) -> String {
        alloc::format!(
            "{{\"seq\":{},\"event\":{}{},\"timestamp\":{},\"prev_hash\":\"sha256:{}\",\"hash\":\"sha256:{}\"}}",
            self.seq,
            json_str(&self.event),
            // body already contains the event fields; we don't reuse the body fields
            // here twice — the body is the canonical pre-image, the line is the rendering.
            self.field_suffix(),
            self.timestamp.0,
            hex(&self.prev_hash),
            hex(&self.hash)
        )
    }

    // The fields come from the body (everything after seq/event up to timestamp). We reuse
    // the body as the source of truth; for the rendering we pluck the event fields out of it.
    fn field_suffix(&self) -> String {
        // body = {"seq":..,"event":".."<fields>,"timestamp":..}
        // We only want <fields>. Find the part between the event string and ,"timestamp".
        if let Some(ts_pos) = self.body.rfind(",\"timestamp\"") {
            if let Some(ev_pos) = self.body.find("\"event\":") {
                // ev_pos points at "event": ; jump past the event string value.
                let after_key = &self.body[ev_pos + 8..ts_pos];
                // after_key = "Name"<fields> → remove the first JSON string.
                if let Some(close) = after_key[1..].find('"') {
                    return after_key[close + 2..].to_string();
                }
            }
        }
        String::new()
    }
}

/// The append-only audit log with hash chain.
#[derive(Clone, Debug, Default)]
pub struct AuditLog {
    entries: Vec<AuditEntry>,
    last_hash: [u8; 32],
}

impl AuditLog {
    pub fn new() -> Self {
        AuditLog { entries: Vec::new(), last_hash: [0u8; 32] }
    }

    /// Append an event at the end and seal it into the chain.
    pub fn append(&mut self, event: &AuditEvent, ts: Timestamp) -> &AuditEntry {
        let seq = self.entries.len() as u64;
        let body = alloc::format!(
            "{{\"seq\":{},\"event\":{}{},\"timestamp\":{}}}",
            seq,
            json_str(event.name()),
            event.fields_json(),
            ts.0
        );
        let prev_hash = self.last_hash;
        let hash = AuditEntry::compute_hash(seq, &prev_hash, &body);
        self.last_hash = hash;
        self.entries.push(AuditEntry {
            seq,
            event: event.name().to_string(),
            timestamp: ts,
            body,
            prev_hash,
            hash,
        });
        self.entries.last().unwrap()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[AuditEntry] {
        &self.entries
    }

    /// The root hash (hash of the last record) — a fingerprint of the log.
    pub fn root_hash(&self) -> [u8; 32] {
        self.last_hash
    }

    /// Verify the integrity of the whole chain. `Err(seq)` = first broken link.
    pub fn verify_chain(&self) -> Result<(), u64> {
        let mut prev = [0u8; 32];
        for e in &self.entries {
            // 1. The stored prev_hash must be the hash of the previous record.
            if e.prev_hash != prev {
                return Err(e.seq);
            }
            // 2. The stored hash must recompute from the body.
            let recomputed = AuditEntry::compute_hash(e.seq, &e.prev_hash, &e.body);
            if recomputed != e.hash {
                return Err(e.seq);
            }
            prev = e.hash;
        }
        Ok(())
    }

    /// All lines (JSON) — for persistence to `/var/log/euro/audit.log`.
    pub fn lines(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.line()).collect()
    }

    /// Filter by event name (substring match, for `eurousers audit --events`).
    pub fn filter_event<'a>(&'a self, names: &[&str]) -> Vec<&'a AuditEntry> {
        self.entries
            .iter()
            .filter(|e| names.iter().any(|n| e.event.eq_ignore_ascii_case(n)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_log() -> AuditLog {
        let mut log = AuditLog::new();
        log.append(&AuditEvent::SystemInit, Timestamp(0));
        log.append(
            &AuditEvent::UserCreated {
                uid: UserId(1000),
                username: "alice".to_string(),
                created_by: UserId(0),
                groups: alloc::vec![GroupId(100), GroupId(2)],
                caps: crate::CAP_LOGIN | crate::CAP_NET,
            },
            Timestamp(10),
        );
        log.append(
            &AuditEvent::LoginSuccess {
                uid: UserId(1000),
                username: "alice".to_string(),
                session: "8f3a2b1c".to_string(),
                caps: crate::CAP_LOGIN | crate::CAP_NET,
                tty: "/dev/tty1".to_string(),
            },
            Timestamp(20),
        );
        log
    }

    #[test]
    fn intact_chain_verifies() {
        let log = sample_log();
        assert_eq!(log.len(), 3);
        assert_eq!(log.verify_chain(), Ok(()));
    }

    #[test]
    fn tampering_a_past_entry_breaks_the_chain() {
        let mut log = sample_log();
        // Tamper with the body of record 1 (the useradd) — e.g. raise caps.
        // Without updating the hash, the recomputation fails on exactly that record.
        log.entries[1].body = log.entries[1].body.replace("alice", "mallory");
        assert_eq!(log.verify_chain(), Err(1));
    }

    #[test]
    fn tampering_body_and_hash_still_breaks_link() {
        let mut log = sample_log();
        // More advanced: also update the hash so record 1 itself is consistent...
        let forged = AuditEntry::compute_hash(1, &log.entries[1].prev_hash, "forged-body");
        log.entries[1].body = "forged-body".to_string();
        log.entries[1].hash = forged;
        // ...but record 2 still refers to the OLD hash → the chain breaks at seq 2.
        assert_eq!(log.verify_chain(), Err(2));
    }

    #[test]
    fn line_renders_event_fields() {
        let log = sample_log();
        let line = log.entries()[1].line();
        assert!(line.contains("\"event\":\"UserCreated\""));
        assert!(line.contains("\"uid\":1000"));
        assert!(line.contains("\"username\":\"alice\""));
        assert!(line.contains("\"prev_hash\":\"sha256:"));
        assert!(line.contains("\"hash\":\"sha256:"));
    }

    #[test]
    fn filter_by_event() {
        let log = sample_log();
        let logins = log.filter_event(&["LoginSuccess"]);
        assert_eq!(logins.len(), 1);
        assert_eq!(logins[0].seq, 2);
    }
}
