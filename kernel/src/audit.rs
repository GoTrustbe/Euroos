//! P3: **append-only audit log** — tamper-evident recording of security events.
//!
//! GDPR/NIS2 require a reliable, non-forgeable record of who did what. We keep
//! the events in an in-memory ring AND persist them to
//! `/var/log/audit.log`, which is marked with the L1 `FLAG_APPEND_ONLY` flag: the
//! file system then allows ONLY extension — earlier lines cannot be erased or
//! modified, not even by root. Clearing that flag requires
//! `CAP_IMMUTABLE_ADMIN` (L2). This way the audit trail is structurally irreversible.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;

use eurofs::{FileSystem, FLAG_APPEND_ONLY};

/// A security event category.
#[derive(Clone, Copy)]
pub enum Event {
    ImmutableSet,
    ImmutableCleared,
    ImmutableDenied,
    CapDenied,
    Login,
    Logout,
    Boot,
    /// One EuroAgent MCP tool call (allowed or denied). The persistent
    /// trail of what an agent did — survives a restart (P0.3 / audit #7).
    AgentTool,
}

impl Event {
    fn tag(self) -> &'static str {
        match self {
            Event::ImmutableSet => "IMMUTABLE_SET",
            Event::ImmutableCleared => "IMMUTABLE_CLEARED",
            Event::ImmutableDenied => "IMMUTABLE_DENIED",
            Event::CapDenied => "CAP_DENIED",
            Event::Login => "LOGIN",
            Event::Logout => "LOGOUT",
            Event::Boot => "BOOT",
            Event::AgentTool => "AGENT_TOOL",
        }
    }
}

const LOG_PATH: &str = "/var/log/audit.log";

static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
static SEQ: AtomicU64 = AtomicU64::new(0);
/// Number of in-memory events already written to disk (so that we APPEND only the
/// NEW events — the on-disk log grows monotonically across boots).
static PERSISTED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Record an event in the in-memory audit ring (lock-protected; safe from any
/// kernel context). Persisting to disk is done later by [`persist`].
pub fn record(event: Event, detail: &str) {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let t = crate::interrupts::ticks();
    let line = format!("{seq:>6} t={t:>8} {} {detail}", event.tag());
    LOG.lock().push(line);
}

/// Number of recorded events.
pub fn count() -> usize {
    LOG.lock().len()
}

/// The last `n` audit lines (for a shell `audit` command).
pub fn recent(n: usize) -> Vec<String> {
    let log = LOG.lock();
    let start = log.len().saturating_sub(n);
    log[start..].to_vec()
}

/// Persist the NEW (not yet written) events to the append-only
/// `/var/log/audit.log`: read the existing content (previous boots + earlier persists)
/// and APPEND only the new lines — this way the write always extends (passes the
/// append-only FS check) and the trail grows monotonically. Set the
/// `FLAG_APPEND_ONLY` flag once (cap-gated via L2). Returns true on success.
pub fn persist(fs: &mut dyn FileSystem, caps: u64) -> bool {
    use core::sync::atomic::Ordering;
    let _ = fs.create_dir("/var");
    let _ = fs.create_dir("/var/log");

    let (new_lines, total) = {
        let log = LOG.lock();
        let already = PERSISTED.load(Ordering::Relaxed).min(log.len());
        let mut s = Vec::new();
        for l in &log[already..] {
            s.extend_from_slice(l.as_bytes());
            s.push(b'\n');
        }
        (s, log.len())
    };
    if new_lines.is_empty() && fs.exists(LOG_PATH) {
        return true; // nothing new + file already exists
    }
    // Existing on-disk content + the new events → strictly extending write.
    let mut body = fs.read_file(LOG_PATH).unwrap_or_default();
    body.extend_from_slice(&new_lines);
    if fs.write_file(LOG_PATH, &body).is_err() {
        return false;
    }
    PERSISTED.store(total, Ordering::Relaxed);
    if fs.get_flags(LOG_PATH).unwrap_or(0) & FLAG_APPEND_ONLY == 0 {
        let _ = crate::immutable::set_protected(fs, LOG_PATH, FLAG_APPEND_ONLY, caps);
    }
    true
}

/// P3 boot self-test: prove that the audit trail is irreversible — events are
/// recorded, persisted to an append-only file, and an attempt to forge it
/// (truncate/overwrite) is refused by the FS.
pub fn selftest(fs: &mut dyn FileSystem, caps: u64) {
    let nl = |fs: &mut dyn FileSystem| fs.read_file(LOG_PATH).map(|d| d.iter().filter(|&&b| b == b'\n').count()).unwrap_or(0);
    record(Event::Boot, "kernel-start");
    record(Event::Login, "user=root tty=console");
    let persisted = persist(fs, caps);
    let append_only = fs.get_flags(LOG_PATH).unwrap_or(0) & FLAG_APPEND_ONLY != 0;
    let lines_before = nl(fs);

    // Tamper attempt: truncate or overwrite the log → the append-only FS refuses.
    let tamper_blocked = fs.write_file(LOG_PATH, b"X").is_err();

    // A new event + re-persist MUST succeed (it only extends) and the
    // on-disk log grows (also works across reboots, since we append instead of rewrite).
    record(Event::ImmutableSet, "/bin/hello");
    let extend_ok = persist(fs, caps);
    let lines_after = nl(fs);

    let ok = persisted && append_only && tamper_blocked && extend_ok && lines_after > lines_before;
    crate::serial_println!(
        "[p3] append-only audit log: {} events, persisted={}, append-only-flag={}, tamper-blocked={}, extend-OK={}, lines-on-disk {}→{} → {}",
        count(), persisted, append_only, tamper_blocked, extend_ok, lines_before, lines_after,
        if ok { "OK (tamper-evident audit trail) ✓" } else { "FAILED" }
    );
}
