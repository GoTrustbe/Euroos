//! L1 + L2: file **immutability** + the **`CAP_IMMUTABLE_ADMIN`** gate.
//!
//! The sovereign security backbone starts here. EuroFS carries per-inode
//! immutability flags (L1: [`eurofs::FLAG_IMMUTABLE`] / [`eurofs::FLAG_APPEND_ONLY`])
//! that block writing/deleting/renaming IN THE FILESYSTEM — independent
//! of POSIX permissions or root. This module is the kernel gate above it (L2): the
//! **setting or clearing** of those flags requires the separate capability
//! [`CAP_IMMUTABLE_ADMIN`]. So even a root shell cannot unlock system files
//! without that explicit, auditable privilege — the foundation for a
//! verifiably immutable system (and, with L3, for verity partitions).

use eurofs::{FileSystem, FsError, FLAG_APPEND_ONLY, FLAG_IMMUTABLE};

use crate::ring3::CAP_IMMUTABLE_ADMIN;

/// L2: set/clear the immutability flags of `path` — ONLY if `caps` contains the
/// `CAP_IMMUTABLE_ADMIN` bit. Otherwise `PermissionDenied`, even for root.
pub fn set_protected(fs: &mut dyn FileSystem, path: &str, flags: u32, caps: u64) -> Result<(), FsError> {
    if caps & CAP_IMMUTABLE_ADMIN == 0 {
        crate::audit::record(crate::audit::Event::ImmutableDenied, path);
        return Err(FsError::PermissionDenied);
    }
    let r = fs.set_flags(path, flags);
    if r.is_ok() {
        crate::audit::record(
            if flags & FLAG_IMMUTABLE != 0 {
                crate::audit::Event::ImmutableSet
            } else {
                crate::audit::Event::ImmutableCleared
            },
            path,
        );
    }
    r
}

/// Mark the bundled system binaries + critical config IMMUTABLE — tamper-proof
/// system files. Returns the number of protected files.
pub fn protect_system_files(fs: &mut dyn FileSystem, caps: u64) -> usize {
    let mut n = 0;
    for &p in SYSTEM_FILES {
        if fs.exists(p) && set_protected(fs, p, FLAG_IMMUTABLE, caps).is_ok() {
            n += 1;
        }
    }
    // Recursive: EVERY file under /bin and /lib is system code — all immutable.
    // (/etc is NOT recursive: runtime state lives there — /etc/euroid,
    // /etc/euroca, /etc/fde must stay writable for the services that own it.)
    for root in ["/bin", "/lib"] {
        n += protect_tree(fs, root, caps, 0);
    }
    harden_modes(fs);
    n
}

/// Recursively immutable-flag every file under `dir` (bounded depth).
fn protect_tree(fs: &mut dyn FileSystem, dir: &str, caps: u64, depth: u32) -> usize {
    if depth > 8 {
        return 0;
    }
    let mut n = 0;
    let entries = match fs.list_dir(dir) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    for e in entries {
        let p = if dir == "/" {
            alloc::format!("/{}", e.name)
        } else {
            alloc::format!("{dir}/{}", e.name)
        };
        match e.kind {
            eurofs::EntryKind::Directory => n += protect_tree(fs, &p, caps, depth + 1),
            _ => {
                let already = fs.get_flags(&p).map(|f| f & FLAG_IMMUTABLE != 0).unwrap_or(false);
                if !already && set_protected(fs, &p, FLAG_IMMUTABLE, caps).is_ok() {
                    n += 1;
                }
            }
        }
    }
    n
}

/// rwx hardening at boot: secrets become 0600 (they were world-readable!),
/// shared scratch becomes 1777 so user sessions can use /tmp.
fn harden_modes(fs: &mut dyn FileSystem) {
    for p in ["/etc/shadow", "/etc/euroid/users.db"] {
        if fs.exists(p) {
            let _ = fs.chmod(p, 0o600);
        }
    }
    for p in ["/tmp", "/var/tmp"] {
        if fs.exists(p) {
            let _ = fs.chmod(p, 0o1777);
        }
    }
}

/// The bundled, tamper-protected system files (mirrored in
/// [`protect_system_files`]) — for the `euroimmutable list` view.
const SYSTEM_FILES: &[&str] = &[
    "/bin/hello",
    "/bin/cat",
    "/bin/dyntest",
    "/lib/libeuro.so",
    "/etc/shadow",
    "/etc/hostname",
];

fn describe_flags(flags: u32) -> &'static str {
    if flags & FLAG_IMMUTABLE != 0 && flags & FLAG_APPEND_ONLY != 0 {
        "immutable + append-only"
    } else if flags & FLAG_IMMUTABLE != 0 {
        "immutable (i)"
    } else if flags & FLAG_APPEND_ONLY != 0 {
        "append-only (a)"
    } else {
        "mutable"
    }
}

/// `euroimmutable` — the privileged immutability admin tool (L2 API). The SETTING/CLEARING
/// of flags goes through [`set_protected`] and thus requires `CAP_IMMUTABLE_ADMIN`; this
/// is the signed admin tool that holds that capability. Reading status is free.
///
/// Subcommands: `status <path>` · `list` · `lock <path>` (+i) · `unlock <path>` (−i).
pub fn shell(fs: &mut dyn FileSystem, sub: &str, path: &str) -> alloc::vec::Vec<alloc::string::String> {
    use alloc::string::ToString;
    use alloc::vec;
    match sub {
        "" | "help" => vec![
            "euroimmutable — immutability (L1/L2):".to_string(),
            "  status <path>  show the immutability flags of a file".to_string(),
            "  list           show the protected system files".to_string(),
            "  lock <path>    mark immutable (+i) — requires CAP_IMMUTABLE_ADMIN".to_string(),
            "  unlock <path>  clear the flags (−i) — requires CAP_IMMUTABLE_ADMIN".to_string(),
        ],
        "status" => {
            if path.is_empty() {
                return vec!["usage: euroimmutable status <path>".to_string()];
            }
            match fs.get_flags(path) {
                Ok(f) => vec![alloc::format!("{path}: {} (flags={f:#x})", describe_flags(f))],
                Err(_) => vec![alloc::format!("euroimmutable: cannot read '{path}'")],
            }
        }
        "list" => {
            let mut out = vec!["protected system files:".to_string()];
            for &p in SYSTEM_FILES {
                if fs.exists(p) {
                    let f = fs.get_flags(p).unwrap_or(0);
                    out.push(alloc::format!("  {p}  →  {}", describe_flags(f)));
                }
            }
            out
        }
        "lock" | "unlock" => {
            if path.is_empty() {
                return vec![alloc::format!("usage: euroimmutable {sub} <path>")];
            }
            let flags = if sub == "lock" { FLAG_IMMUTABLE } else { 0 };
            match set_protected(fs, path, flags, CAP_IMMUTABLE_ADMIN) {
                Ok(()) => vec![alloc::format!(
                    "euroimmutable: {path} is now {} (audited)",
                    describe_flags(flags)
                )],
                Err(_) => vec![alloc::format!(
                    "euroimmutable: DENIED for {path} — requires CAP_IMMUTABLE_ADMIN"
                )],
            }
        }
        _ => vec![alloc::format!("euroimmutable: unknown subcommand '{sub}' (see: euroimmutable help)")],
    }
}

/// L1/L2 boot self-test: prove (a) the cap gate on setting the flag, and (b) that
/// the FS layer really protects an immutable file against writing/deleting.
pub fn selftest(fs: &mut dyn FileSystem) {
    let path = "/tmp/l1-test";
    let _ = fs.create_dir("/tmp");
    if fs.write_file(path, b"origineel").is_err() {
        crate::serial_println!("[l1] self-test: could not create test file");
        return;
    }

    // (L2) Without CAP_IMMUTABLE_ADMIN the flag must NOT be settable — not even "as root".
    let no_cap = set_protected(fs, path, FLAG_IMMUTABLE, crate::ring3::CAP_FILE);
    // (L2) With the capability it does succeed.
    let with_cap = set_protected(fs, path, FLAG_IMMUTABLE, CAP_IMMUTABLE_ADMIN);

    // (L1) Now immutable: writing + deleting are rejected by the FS.
    let write_blocked = fs.write_file(path, b"gehackt") == Err(FsError::PermissionDenied);
    let remove_blocked = fs.remove_file(path) == Err(FsError::PermissionDenied);
    let intact = fs.read_file(path).map(|d| d == b"origineel").unwrap_or(false);

    // (L2) Clearing the flag also requires the capability; afterwards modifiable again.
    let clear_no_cap = set_protected(fs, path, 0, crate::ring3::CAP_FILE) == Err(FsError::PermissionDenied);
    let _ = set_protected(fs, path, 0, CAP_IMMUTABLE_ADMIN);
    let writable_again = fs.write_file(path, b"weer-mutabel").is_ok();
    let _ = fs.remove_file(path);

    let ok = no_cap == Err(FsError::PermissionDenied)
        && with_cap.is_ok()
        && write_blocked
        && remove_blocked
        && intact
        && clear_no_cap
        && writable_again;
    crate::serial_println!(
        "[l1] immutability + CAP_IMMUTABLE_ADMIN: cap-gate-on-set={}, write-blocked={}, delete-blocked={}, content-intact={}, cap-gate-on-clear={}, mutable-again-after-clear={} → {}",
        no_cap == Err(FsError::PermissionDenied), write_blocked, remove_blocked, intact, clear_no_cap, writable_again,
        if ok { "OK (even root cannot change anything without the cap) ✓" } else { "FAILED" }
    );
    let _ = FLAG_APPEND_ONLY; // (P3 uses this flag — see audit.rs)
}
