//! EuroAuth: the active SESSION (uid/gid/name) + the POSIX identity mapping from
//! /etc/passwd. **Password verification has, since Sprint AE, run via EuroID**
//! ([`crate::euroid::login`], Argon2id memory-hard + lockout + tamper-evident
//! audit) — no longer via iterated SHA-256 here. `shadow_line`/`hash` remain
//! only to seed /etc/shadow as a Linux-compat artifact (not the login path).
//! The session determines what getuid() etc. return; `login`/`su`/`sudo` mutate it.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use eurofs::FileSystem;
use spin::Mutex;

const ITER: u32 = 4096; // iteration count (stretch factor against brute-force)

static SESSION_UID: AtomicU32 = AtomicU32::new(1000); // desktop session starts as 'euro'
static SESSION_GID: AtomicU32 = AtomicU32::new(1000);
static SESSION_NAME: Mutex<String> = Mutex::new(String::new());

/// Salted, iterated SHA-256 hash: h0 = SHA256(salt||password), then
/// hᵢ = SHA256(salt||hᵢ₋₁), ITER times.
pub fn hash(salt: &[u8], password: &[u8]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(salt.len() + password.len());
    buf.extend_from_slice(salt);
    buf.extend_from_slice(password);
    let mut h = eurotls::keyschedule::sha256(&buf);
    for _ in 1..ITER {
        let mut b = Vec::with_capacity(salt.len() + 32);
        b.extend_from_slice(salt);
        b.extend_from_slice(&h);
        h = eurotls::keyschedule::sha256(&b);
    }
    h
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

/// Build an /etc/shadow line for installation: `user:salt_hex:hash_hex`. Only
/// still used to seed /etc/shadow as a Linux-compat artifact — NOT the login path
/// (that is [`crate::euroid::login`], Argon2id).
pub fn shadow_line(user: &str, salt: &[u8], password: &[u8]) -> String {
    alloc::format!("{user}:{}:{}", to_hex(salt), to_hex(&hash(salt, password)))
}

/// Look up the (uid, gid) of a user in /etc/passwd (`name:x:uid:gid:...`).
pub fn lookup_user(fs: &mut dyn FileSystem, user: &str) -> Option<(u32, u32)> {
    let data = fs.read_file("/etc/passwd").ok()?;
    let s = core::str::from_utf8(&data).ok()?;
    for line in s.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 4 && f[0] == user {
            return Some((f[2].parse().ok()?, f[3].parse().ok()?));
        }
    }
    None
}

/// Name of the user with `uid` (from /etc/passwd), or "uid<N>".
pub fn name_for_uid(fs: &mut dyn FileSystem, uid: u32) -> String {
    if let Ok(data) = fs.read_file("/etc/passwd") {
        if let Ok(s) = core::str::from_utf8(&data) {
            for line in s.lines() {
                let f: Vec<&str> = line.split(':').collect();
                if f.len() >= 3 && f[2].parse::<u32>() == Ok(uid) {
                    return String::from(f[0]);
                }
            }
        }
    }
    alloc::format!("uid{uid}")
}

pub fn session_uid() -> u32 {
    SESSION_UID.load(Ordering::Relaxed)
}
pub fn session_gid() -> u32 {
    SESSION_GID.load(Ordering::Relaxed)
}

/// Name of the active session (empty before login).
pub fn session_name() -> String {
    SESSION_NAME.lock().clone()
}

/// Avatar initials for the current session: the first 2 letters of the
/// username (uppercase), or "EU" if there is no session yet. NEVER contains
/// hardcoded personal data — it is derived from the logged-in user.
pub fn session_initials() -> String {
    let name = session_name();
    let up: String = name.chars().take(2).collect::<String>().to_uppercase();
    if up.is_empty() { String::from("EU") } else { up }
}

/// Set the active session (after login/su).
pub fn set_session(uid: u32, gid: u32, name: &str) {
    SESSION_UID.store(uid, Ordering::Relaxed);
    SESSION_GID.store(gid, Ordering::Relaxed);
    *SESSION_NAME.lock() = String::from(name);
}
