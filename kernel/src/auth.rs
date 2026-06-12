//! EuroAuth: de actieve SESSIE (uid/gid/naam) + de POSIX-identiteitsmapping uit
//! /etc/passwd. **Wachtwoord-verificatie loopt sinds Sprint AE via EuroID**
//! ([`crate::euroid::login`], Argon2id memory-hard + lockout + tamper-evident
//! audit) — niet meer via geïtereerde SHA-256 hier. `shadow_line`/`hash` blijven
//! enkel om /etc/shadow als Linux-compat-artefact te zaaien (geen login-pad).
//! De sessie bepaalt wat getuid() e.d. teruggeven; `login`/`su`/`sudo` muteren haar.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use eurofs::FileSystem;
use spin::Mutex;

const ITER: u32 = 4096; // iteratie-telling (rek-factor tegen brute-force)

static SESSION_UID: AtomicU32 = AtomicU32::new(1000); // desktop-sessie start als 'euro'
static SESSION_GID: AtomicU32 = AtomicU32::new(1000);
static SESSION_NAME: Mutex<String> = Mutex::new(String::new());

/// Gezouten, geïtereerde SHA-256-hash: h0 = SHA256(salt||wachtwoord), daarna
/// hᵢ = SHA256(salt||hᵢ₋₁), ITER keer.
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

/// Bouw een /etc/shadow-regel voor installatie: `user:salt_hex:hash_hex`. Enkel
/// nog om /etc/shadow als Linux-compat-artefact te zaaien — NIET het login-pad
/// (dat is [`crate::euroid::login`], Argon2id).
pub fn shadow_line(user: &str, salt: &[u8], password: &[u8]) -> String {
    alloc::format!("{user}:{}:{}", to_hex(salt), to_hex(&hash(salt, password)))
}

/// Zoek (uid, gid) van een gebruiker in /etc/passwd (`naam:x:uid:gid:...`).
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

/// Naam van de gebruiker met `uid` (uit /etc/passwd), of "uid<N>".
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

/// Zet de actieve sessie (na login/su).
pub fn set_session(uid: u32, gid: u32, name: &str) {
    SESSION_UID.store(uid, Ordering::Relaxed);
    SESSION_GID.store(gid, Ordering::Relaxed);
    *SESSION_NAME.lock() = String::from(name);
}
