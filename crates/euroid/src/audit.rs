//! Append-only **hash-chain audit-log** (P3 + GDPR Art. 5(2)/32, NIS2 Art. 21).
//!
//! Elke gebruikersactie wordt als zelf-beschrijvend JSON-record vastgelegd. Elk
//! record bevat de hash van het vórige record (`prev_hash`) plus zijn eigen `hash`
//! over `seq ‖ prev_hash ‖ body`. Daardoor maakt élke wijziging aan een ouder
//! record alle volgende hashes ongeldig — het log is tamper-evident, zélfs zonder
//! de EuroFS APPEND_ONLY-vlag (die het bovendien fysiek onomkeerbaar maakt).

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

/// Waarom een aanmelding geweigerd werd (zonder te onthullen óf de gebruiker bestaat).
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

/// Elk audit-event. Wordt als JSON naar het append-only log geserialiseerd.
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
    /// De event-naam (`"event"`-veld).
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

    /// De event-specifieke velden als JSON-fragment (zonder accolades).
    /// GDPR Art. 32 pseudonimisatie: we loggen UID's, geen namen-als-sleutel.
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

/// Eén record in de keten.
#[derive(Clone, Debug)]
pub struct AuditEntry {
    pub seq: u64,
    pub event: String,
    pub timestamp: Timestamp,
    /// De geserialiseerde body (`seq` + `event` + velden + `timestamp`) waarover gehasht wordt.
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

    /// De volledige JSON-regel zoals die op schijf staat (incl. prev_hash/hash).
    pub fn line(&self) -> String {
        alloc::format!(
            "{{\"seq\":{},\"event\":{}{},\"timestamp\":{},\"prev_hash\":\"sha256:{}\",\"hash\":\"sha256:{}\"}}",
            self.seq,
            json_str(&self.event),
            // body bevat al de event-velden; we hergebruiken de body-velden hier niet
            // dubbel — de body is het canonieke pre-image, de regel is de weergave.
            self.field_suffix(),
            self.timestamp.0,
            hex(&self.prev_hash),
            hex(&self.hash)
        )
    }

    // De velden komen uit de body (alles na seq/event tot timestamp). We hergebruiken
    // de body als bron van waarheid; voor de weergave plukken we de event-velden eruit.
    fn field_suffix(&self) -> String {
        // body = {"seq":..,"event":".."<velden>,"timestamp":..}
        // We willen alleen <velden>. Vind het stuk tussen de event-string en ,"timestamp".
        if let Some(ts_pos) = self.body.rfind(",\"timestamp\"") {
            if let Some(ev_pos) = self.body.find("\"event\":") {
                // ev_pos wijst naar "event": ; spring voorbij de event-stringwaarde.
                let after_key = &self.body[ev_pos + 8..ts_pos];
                // after_key = "Naam"<velden> → verwijder de eerste JSON-string.
                if let Some(close) = after_key[1..].find('"') {
                    return after_key[close + 2..].to_string();
                }
            }
        }
        String::new()
    }
}

/// Het append-only audit-log met hash-keten.
#[derive(Clone, Debug, Default)]
pub struct AuditLog {
    entries: Vec<AuditEntry>,
    last_hash: [u8; 32],
}

impl AuditLog {
    pub fn new() -> Self {
        AuditLog { entries: Vec::new(), last_hash: [0u8; 32] }
    }

    /// Voeg een event achteraan toe en sluit het in de keten.
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

    /// De wortelhash (hash van het laatste record) — een vingerafdruk van het log.
    pub fn root_hash(&self) -> [u8; 32] {
        self.last_hash
    }

    /// Verifieer de integriteit van de hele keten. `Err(seq)` = eerste kapotte schakel.
    pub fn verify_chain(&self) -> Result<(), u64> {
        let mut prev = [0u8; 32];
        for e in &self.entries {
            // 1. De opgeslagen prev_hash moet de hash van het vorige record zijn.
            if e.prev_hash != prev {
                return Err(e.seq);
            }
            // 2. De opgeslagen hash moet de body opnieuw uitrekenen.
            let recomputed = AuditEntry::compute_hash(e.seq, &e.prev_hash, &e.body);
            if recomputed != e.hash {
                return Err(e.seq);
            }
            prev = e.hash;
        }
        Ok(())
    }

    /// Alle regels (JSON) — voor persistentie naar `/var/log/euro/audit.log`.
    pub fn lines(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.line()).collect()
    }

    /// Filter op event-naam (substring-match, voor `eurousers audit --events`).
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
        // Manipuleer de body van record 1 (de useradd) — bv. caps verhogen.
        // Zonder de hash bij te werken faalt de her-berekening op exact dat record.
        log.entries[1].body = log.entries[1].body.replace("alice", "mallory");
        assert_eq!(log.verify_chain(), Err(1));
    }

    #[test]
    fn tampering_body_and_hash_still_breaks_link() {
        let mut log = sample_log();
        // Geavanceerder: werk óók de hash bij zodat record 1 zelf klopt...
        let forged = AuditEntry::compute_hash(1, &log.entries[1].prev_hash, "forged-body");
        log.entries[1].body = "forged-body".to_string();
        log.entries[1].hash = forged;
        // ...maar record 2 verwijst nog naar de OUDE hash → de keten breekt bij seq 2.
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
