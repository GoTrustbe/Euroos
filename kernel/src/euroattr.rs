//! 3D-5 — **user-scoped file immutability** (`euroattr`).
//!
//! L1/L2 immutability ([`crate::immutable`]) is admin-only: setting a flag needs
//! `CAP_IMMUTABLE_ADMIN`, so a normal user cannot protect their *own* files.
//! EuroAttr closes that: a user may set/clear `IMMUTABLE` / `APPEND_ONLY` on
//! files **inside their own home directory** without any elevated capability,
//! gated purely by ownership. System paths still require the admin cap, so this
//! adds user power without weakening the system's immutability.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use eurofs::{FileSystem, FsError, FLAG_APPEND_ONLY, FLAG_IMMUTABLE};

fn home_prefix(user: &str) -> String {
    alloc::format!("/home/{user}/")
}

/// A user may change attributes only on files under their own home directory.
pub fn owns(path: &str, user: &str) -> bool {
    path.starts_with(&home_prefix(user))
}

/// Set the immutability `flags` of `path` **as** `user` — no admin capability
/// required, but only for files the user owns. Non-owned / system paths are
/// refused (they still need [`crate::immutable::set_protected`]).
pub fn set_user(fs: &mut dyn FileSystem, path: &str, flags: u32, user: &str) -> Result<(), FsError> {
    if !owns(path, user) {
        crate::audit::record(crate::audit::Event::ImmutableDenied, path);
        return Err(FsError::PermissionDenied);
    }
    let r = fs.set_flags(path, flags);
    if r.is_ok() {
        crate::audit::record(
            if flags & (FLAG_IMMUTABLE | FLAG_APPEND_ONLY) != 0 {
                crate::audit::Event::ImmutableSet
            } else {
                crate::audit::Event::ImmutableCleared
            },
            path,
        );
    }
    r
}

/// `[3d5]` boot self-test on an isolated in-memory EuroFS.
pub fn selftest() {
    use eurofs::{EuroFs, MemoryBlockDevice};
    let dev = MemoryBlockDevice::new(4096, 4096);
    let mut fs = match EuroFs::format(dev, [0xA5; 16], crate::rtc::epoch()) {
        Ok(f) => f,
        Err(_) => {
            crate::serial_println!("[3d5] EuroAttr: FS format failed");
            return;
        }
    };
    let _ = fs.create_dir("/home");
    let _ = fs.create_dir("/home/alice");
    let _ = fs.write_file("/home/alice/notes.txt", b"my notes");

    // alice (no admin cap) makes her OWN file immutable.
    let own_set = set_user(&mut fs, "/home/alice/notes.txt", FLAG_IMMUTABLE, "alice").is_ok();
    // A write is now refused by the filesystem itself.
    let write_blocked = fs.write_file("/home/alice/notes.txt", b"tampered").is_err();
    // alice CANNOT lock a system file or another user's file (not the owner).
    let system_denied = matches!(set_user(&mut fs, "/etc/passwd", FLAG_IMMUTABLE, "alice"), Err(FsError::PermissionDenied));
    let other_denied = matches!(set_user(&mut fs, "/home/bob/secret", FLAG_IMMUTABLE, "alice"), Err(FsError::PermissionDenied));
    // The owner can clear it again → writing works.
    let cleared = set_user(&mut fs, "/home/alice/notes.txt", 0, "alice").is_ok();
    let write_ok = fs.write_file("/home/alice/notes.txt", b"edited freely").is_ok();

    let ok = own_set && write_blocked && system_denied && other_denied && cleared && write_ok;
    crate::serial_println!(
        "[3d5] EuroAttr user immutability (no admin cap on your own files): set-own-immutable={own_set}, write-then-blocked={write_blocked}, system-path-denied={system_denied}, other-user-denied={other_denied}, owner-can-clear={cleared}, write-after-clear={write_ok} → {}",
        if ok { "OK (users protect their own files; system files still need CAP_IMMUTABLE_ADMIN) ✓" } else { "FAILED" }
    );
}

/// `euroattr` shell command: manage immutability on your own home files.
/// Usage: `euroattr +i|-i|+a|-a <path>` or `euroattr status <path>`.
pub fn shell(args: &str, user: &str, fs: &mut dyn FileSystem) -> Vec<String> {
    let mut a = args.split_whitespace();
    let op = a.next().unwrap_or("");
    let path = a.next().unwrap_or("");
    if path.is_empty() {
        return alloc::vec!["usage: euroattr +i|-i|+a|-a <path>  |  euroattr status <path>".to_string()];
    }
    match op {
        "status" => match fs.get_flags(path) {
            Ok(f) => alloc::vec![alloc::format!(
                "{path}: immutable={} append-only={}",
                f & FLAG_IMMUTABLE != 0,
                f & FLAG_APPEND_ONLY != 0
            )],
            Err(e) => alloc::vec![alloc::format!("{path}: {e:?}")],
        },
        "+i" | "-i" | "+a" | "-a" => {
            let cur = fs.get_flags(path).unwrap_or(0);
            let bit = if op.ends_with('i') { FLAG_IMMUTABLE } else { FLAG_APPEND_ONLY };
            let flags = if op.starts_with('+') { cur | bit } else { cur & !bit };
            match set_user(fs, path, flags, user) {
                Ok(()) => alloc::vec![alloc::format!("{path}: attributes updated (immutable/append-only)")],
                Err(FsError::PermissionDenied) => {
                    alloc::vec![alloc::format!("{path}: EPERM — you can only change attributes under /home/{user}/")]
                }
                Err(e) => alloc::vec![alloc::format!("{path}: {e:?}")],
            }
        }
        _ => alloc::vec!["usage: euroattr +i|-i|+a|-a <path>  |  euroattr status <path>".to_string()],
    }
}
