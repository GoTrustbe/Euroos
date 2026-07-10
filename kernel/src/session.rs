//! **3E-3 session lifecycle** — multi-user sessions on EuroID.
//!
//! Before 3E the "session" was three global atomics (uid/gid/name) that
//! `login`/`su` mutated in place: no history, no lifecycle, no home creation.
//! This module adds the real session model on top of that POSIX mapping:
//! a session TABLE with open/close lifecycle (single-seat: opening a session
//! closes the previous one), auto-creation of `/home/<user>` OWNED by the user
//! (3E-9 uid-on-inode), the per-session FS uid-context (files you create are
//! yours), and audit events for every open/close.
//!
//! Honest scope: this is a single-seat console/desktop model — one session is
//! ACTIVE at a time (like a laptop, not a terminal server). Concurrent sessions
//! (SSH-style) need per-process session binding, which is future work.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use eurofs::{FileSystem, FsError};
use spin::Mutex;

#[derive(Clone)]
pub struct SessionInfo {
    pub id: u64,
    pub uid: u32,
    pub name: String,
    pub caps: u64,
    /// Wall-clock (rtc::epoch) at open.
    pub opened_at: u64,
    /// 0 = still open.
    pub closed_at: u64,
    /// How the session came to be: "login" | "su" | "auto" | "desktop".
    pub how: &'static str,
}

static TABLE: Mutex<Vec<SessionInfo>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// Bound the in-memory history (the durable trail is the audit log).
const MAX_HISTORY: usize = 64;

fn now() -> u64 {
    crate::rtc::epoch()
}

/// Close the active session (if any) → its id. Audited.
pub fn close_active() -> Option<u64> {
    let mut t = TABLE.lock();
    let n = now();
    for s in t.iter_mut().rev() {
        if s.closed_at == 0 {
            s.closed_at = n.max(s.opened_at);
            crate::audit::record(crate::audit::Event::Logout, "session closed");
            // 3F-7: session-scoped portal grants end with the session.
            crate::portal::end_session();
            return Some(s.id);
        }
    }
    None
}

/// Open a session for `name`/`uid`: closes the previous one (single-seat),
/// sets the POSIX session (EuroAuth) + the FS uid-context (new files belong to
/// this user), and ensures `/home/<name>` exists and is OWNED by the user.
pub fn open(fs: &mut dyn FileSystem, uid: u32, gid: u32, name: &str, caps: u64, how: &'static str) -> u64 {
    close_active();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    {
        let mut t = TABLE.lock();
        if t.len() >= MAX_HISTORY {
            // Drop the oldest CLOSED entry (never an open one).
            if let Some(pos) = t.iter().position(|s| s.closed_at != 0) {
                t.remove(pos);
            }
        }
        t.push(SessionInfo { id, uid, name: name.to_string(), caps, opened_at: now(), closed_at: 0, how });
    }
    crate::auth::set_session(uid, gid, name);
    fs.set_uid_context(uid);
    if uid != 0 {
        ensure_home(fs, name, uid);
    }
    crate::audit::record(crate::audit::Event::Login, "session opened");
    id
}

/// The session that is currently open (if any).
pub fn active() -> Option<SessionInfo> {
    TABLE.lock().iter().rev().find(|s| s.closed_at == 0).cloned()
}

/// `/home/<name>` exists and belongs to its user. Also heals pre-3E homes
/// that were created with owner 0 (system).
fn ensure_home(fs: &mut dyn FileSystem, name: &str, uid: u32) {
    if !fs.exists("/home") {
        let _ = fs.create_dir("/home");
    }
    let home = alloc::format!("/home/{name}");
    if !fs.exists(&home) {
        let _ = fs.create_dir(&home);
    }
    if fs.owner(&home).map(|u| u != uid).unwrap_or(false) {
        let _ = fs.chown(&home, uid);
    }
}

/// `sessions` shell command: the session table, newest first.
pub fn list_lines() -> Vec<String> {
    let t = TABLE.lock();
    let mut out = alloc::vec![String::from("ID   USER             UID    VIA      STATE")];
    for s in t.iter().rev() {
        out.push(alloc::format!(
            "{:<4} {:<16} {:<6} {:<8} {}",
            s.id,
            s.name,
            s.uid,
            s.how,
            if s.closed_at == 0 { "open".to_string() } else { alloc::format!("closed (+{}s)", s.closed_at.saturating_sub(s.opened_at)) }
        ));
    }
    if t.is_empty() {
        out.push(String::from("(no sessions yet)"));
    }
    out
}

/// `[3e3]` boot self-test — the full session lifecycle on the LIVE root FS:
/// auto-session → alice logs in (euro closed, /home/alice auto-created and
/// alice-OWNED, files she writes are hers) → back to euro (alice closed).
pub fn selftest(fs: &mut dyn FileSystem) {
    let euro_id = open(fs, 1000, 1000, "euro", 0, "auto");
    let alice_id = open(fs, 1002, 1002, "alice", 0, "login");

    let alice_active = active().map(|s| s.id == alice_id && s.uid == 1002).unwrap_or(false);
    let euro_closed = TABLE.lock().iter().find(|s| s.id == euro_id).map(|s| s.closed_at != 0).unwrap_or(false);
    let home_owned = fs.exists("/home/alice") && fs.owner("/home/alice").ok() == Some(1002);
    // Files created inside the session belong to the session user (FS uid-context).
    let _ = fs.write_file("/home/alice/.session-test", b"alice was here");
    let file_owned = fs.owner("/home/alice/.session-test").ok() == Some(1002);
    let _ = fs.remove_file("/home/alice/.session-test");

    let _ = open(fs, 1000, 1000, "euro", 0, "auto");
    let alice_closed =
        TABLE.lock().iter().find(|s| s.id == alice_id).map(|s| s.closed_at != 0).unwrap_or(false);

    let ok = alice_active && euro_closed && home_owned && file_owned && alice_closed;
    crate::serial_println!(
        "[3e3] session lifecycle: switch-closes-previous={euro_closed}, /home/alice auto-created+alice-owned={home_owned}, session-files-owned-by-user={file_owned}, logout-closes={alice_closed} → {}",
        if ok { "OK (multi-user session model live) ✓" } else { "FAILED ✗" }
    );
}

/// `[3e9]` boot self-test — per-user disk quota on the LIVE root FS: a 2-block
/// limit admits a 2-block file, REFUSES the next one (and the file does not
/// appear), and admits it again after the first is deleted (blocks credited).
pub fn quota_selftest(fs: &mut dyn FileSystem) {
    const QUID: u32 = 4242; // scratch uid — not a real account
    let set_ok = fs.quota_set(QUID, 2).is_ok();
    fs.set_uid_context(QUID);

    let w1 = fs.write_file("/.qtest-1.bin", &alloc::vec![0xAB; 8192]).is_ok(); // 2 blocks
    let info = fs.quota_info(QUID).unwrap_or((0, 0));
    let over = fs.write_file("/.qtest-2.bin", &alloc::vec![0xCD; 8192]); // would be 4 > 2
    let refused = over == Err(FsError::QuotaExceeded) && !fs.exists("/.qtest-2.bin");
    let rm = fs.remove_file("/.qtest-1.bin").is_ok();
    let after_credit = fs.write_file("/.qtest-2.bin", &alloc::vec![0xCD; 8192]).is_ok();

    // Cleanup: files away, limit away, context back to system.
    let _ = fs.remove_file("/.qtest-2.bin");
    let _ = fs.quota_set(QUID, 0);
    fs.set_uid_context(0);

    let ok = set_ok && w1 && info == (2, 2) && refused && rm && after_credit;
    crate::serial_println!(
        "[3e9] per-user disk quota (limit 2 blocks): within-quota-write={w1}, usage/limit={info:?}, over-quota-REFUSED(+no-partial-file)={refused}, delete-credits→write-OK={after_credit} → {}",
        if ok { "OK (quota enforced on the live root FS) ✓" } else { "FAILED ✗" }
    );
}
