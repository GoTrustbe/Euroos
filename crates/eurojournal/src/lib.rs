//! EuroJournal — a **structured system journal** (the journald equivalent).
//!
//! Beyond the security audit log, an OS needs a general, *structured* log: every
//! entry carries a **severity** and a **facility** (subsystem), not just a line
//! of text, so operators can filter ("show me all `err`+ from `net`"), export it
//! as JSON for a SIEM, and keep a bounded ring in RAM that the newest entries
//! never overflow. Pure `no_std`, host-tested; the kernel persists it to disk.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Syslog-style severities (RFC 5424), lower = more severe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Emerg = 0,
    Alert = 1,
    Crit = 2,
    Err = 3,
    Warning = 4,
    Notice = 5,
    Info = 6,
    Debug = 7,
}

impl Severity {
    pub fn name(self) -> &'static str {
        match self {
            Severity::Emerg => "emerg",
            Severity::Alert => "alert",
            Severity::Crit => "crit",
            Severity::Err => "err",
            Severity::Warning => "warning",
            Severity::Notice => "notice",
            Severity::Info => "info",
            Severity::Debug => "debug",
        }
    }
}

/// One structured journal entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub seq: u64,
    pub ts: u64,
    pub severity: Severity,
    pub facility: String,
    pub message: String,
}

/// A bounded journal ring: oldest entries are dropped once `cap` is reached, so
/// memory is fixed while the newest history is always retained.
pub struct Journal {
    ring: VecDeque<Entry>,
    cap: usize,
    next_seq: u64,
    /// How many entries were dropped by the ring bound (so nothing is silently lost).
    pub dropped: u64,
}

impl Journal {
    pub fn new(cap: usize) -> Journal {
        Journal { ring: VecDeque::with_capacity(cap.min(1024)), cap: cap.max(1), next_seq: 0, dropped: 0 }
    }

    /// Append a structured entry; returns its sequence number.
    pub fn log(&mut self, ts: u64, severity: Severity, facility: &str, message: &str) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        if self.ring.len() == self.cap {
            self.ring.pop_front();
            self.dropped += 1;
        }
        self.ring.push_back(Entry { seq, ts, severity, facility: facility.to_string(), message: message.to_string() });
        seq
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Filter by minimum severity (`<= min` = at least this severe) and/or
    /// facility.
    pub fn query(&self, min_severity: Option<Severity>, facility: Option<&str>) -> Vec<&Entry> {
        self.ring
            .iter()
            .filter(|e| min_severity.map(|m| e.severity <= m).unwrap_or(true))
            .filter(|e| facility.map(|f| e.facility == f).unwrap_or(true))
            .collect()
    }

    /// Export the whole journal as a JSON array (for a SIEM / support bundle).
    pub fn to_json(&self) -> String {
        let mut s = String::from("[");
        for (i, e) in self.ring.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&alloc::format!(
                "{{\"seq\":{},\"ts\":{},\"severity\":\"{}\",\"facility\":\"{}\",\"message\":\"{}\"}}",
                e.seq,
                e.ts,
                e.severity.name(),
                esc(&e.facility),
                esc(&e.message),
            ));
        }
        s.push(']');
        s
    }
}

fn esc(s: &str) -> String {
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

    fn j() -> Journal {
        let mut j = Journal::new(100);
        j.log(1, Severity::Info, "boot", "kernel started");
        j.log(2, Severity::Err, "net", "link down");
        j.log(3, Severity::Warning, "fs", "scrub found 1 bad block");
        j.log(4, Severity::Info, "net", "link up");
        j
    }

    #[test]
    fn query_by_severity_and_facility() {
        let j = j();
        // At least Err severe (Err/Crit/Alert/Emerg) → just the "link down".
        assert_eq!(j.query(Some(Severity::Err), None).len(), 1);
        // Everything from facility "net".
        assert_eq!(j.query(None, Some("net")).len(), 2);
        // At least Warning, from net → only "link down" (Err <= Warning).
        assert_eq!(j.query(Some(Severity::Warning), Some("net")).len(), 1);
    }

    #[test]
    fn json_export() {
        let j = j();
        let s = j.to_json();
        assert!(s.contains("\"severity\":\"err\"") && s.contains("\"facility\":\"net\""));
        assert!(s.contains("link down"));
    }

    #[test]
    fn bounded_ring_drops_oldest_and_counts() {
        let mut j = Journal::new(3);
        for i in 0..10 {
            j.log(i, Severity::Info, "spam", "x");
        }
        assert_eq!(j.len(), 3); // capped
        assert_eq!(j.dropped, 7);
        // The retained entries are the newest (seq 7,8,9).
        assert_eq!(j.query(None, None).first().unwrap().seq, 7);
    }
}
