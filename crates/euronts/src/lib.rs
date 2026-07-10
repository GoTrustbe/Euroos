//! EuroNTS — **Network Time Security** (RFC 8915): authenticated NTP.
//!
//! An un-authenticated clock is a soft target: attestation freshness, TLS
//! certificate validity and audit-log ordering all trust "now", and an off-path
//! attacker who can spoof NTP can roll a machine's time backwards to re-open
//! expired certificates or replay tokens. NTS fixes this: after a TLS-based key
//! establishment, every NTP exchange carries a **Unique Identifier** (anti
//! off-path / anti-replay) and an **AEAD authenticator** over the whole packet,
//! so a client accepts a timestamp only if it is cryptographically bound to the
//! server it negotiated keys with.
//!
//! This crate implements the NTPv4 + NTS extension-field protocol and the
//! **RFC 8446 §7.5 TLS exporter** key schedule (so C2S/S2C keys match a real
//! endpoint given the same exporter secret). AEAD = **ChaCha20-Poly1305**
//! (IANA AEAD id 29) — sovereign, already in the EuroOS stack.
//!
//! Scope note: the mandatory-to-implement AEAD_AES_SIV_CMAC_256 (id 15) and the
//! live NTS-KE-over-TLS handshake + real-server sync are the remaining interop
//! pieces; the authenticated-time *protocol core* is here and host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};

/// IANA AEAD id for AEAD_CHACHA20_POLY1305.
pub const AEAD_CHACHA20_POLY1305: u16 = 29;

// NTS extension-field types.
const EF_UNIQUE_ID: u16 = 0x0104;
const EF_COOKIE: u16 = 0x0204;
const EF_AUTH: u16 = 0x0404;

const NTP_HEADER_LEN: usize = 48;
const NONCE_LEN: usize = 12; // ChaCha20-Poly1305

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NtsError {
    Malformed,
    /// The Unique Identifier did not match the request (off-path / wrong reply).
    UniqueIdMismatch,
    /// The AEAD authenticator failed — the packet was forged or tampered.
    AuthFailed,
    MissingAuth,
}

// ── RFC 8446 §7.5 TLS exporter key schedule ────────────────────────────────
fn hkdf_expand_label(secret: &[u8], label: &str, context: &[u8], len: usize) -> Vec<u8> {
    let full = alloc::format!("tls13 {label}");
    let mut info = Vec::new();
    info.extend_from_slice(&(len as u16).to_be_bytes());
    info.push(full.len() as u8);
    info.extend_from_slice(full.as_bytes());
    info.push(context.len() as u8);
    info.extend_from_slice(context);
    let hk = Hkdf::<Sha256>::from_prk(secret).expect("prk");
    let mut out = alloc::vec![0u8; len];
    hk.expand(&info, &mut out).expect("expand");
    out
}
fn derive_secret(secret: &[u8], label: &str, messages: &[u8]) -> Vec<u8> {
    let th = Sha256::digest(messages);
    hkdf_expand_label(secret, label, &th, 32)
}
/// TLS-Exporter(label, context, length) per RFC 8446 §7.5.
fn tls_exporter(exporter_secret: &[u8], label: &str, context: &[u8], len: usize) -> Vec<u8> {
    let s = derive_secret(exporter_secret, label, b"");
    let ctx_hash = Sha256::digest(context);
    hkdf_expand_label(&s, "exporter", &ctx_hash, len)
}

/// Derive the C2S (`c2s=true`) or S2C key from the NTS-KE TLS exporter secret,
/// per RFC 8915 §5.1: label `EXPORTER-network-time-security`, context =
/// `0x0000 ‖ AEAD-id ‖ {0x00|0x01}`.
pub fn derive_key(exporter_secret: &[u8], aead_id: u16, c2s: bool) -> [u8; 32] {
    let mut ctx = [0u8; 5];
    ctx[2..4].copy_from_slice(&aead_id.to_be_bytes());
    ctx[4] = if c2s { 0x00 } else { 0x01 };
    let k = tls_exporter(exporter_secret, "EXPORTER-network-time-security", &ctx, 32);
    let mut out = [0u8; 32];
    out.copy_from_slice(&k);
    out
}

// ── extension-field (de)serialisation ──────────────────────────────────────
fn pad4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

/// Encode one extension field: Type(2) ‖ Length(2, incl. 4-byte header, padded
/// to 4) ‖ Value (zero-padded to 4).
fn ef(ty: u16, value: &[u8]) -> Vec<u8> {
    let body_len = pad4(value.len());
    let total = 4 + body_len;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&ty.to_be_bytes());
    out.extend_from_slice(&(total as u16).to_be_bytes());
    out.extend_from_slice(value);
    out.resize(total, 0);
    out
}

/// Walk the extension fields starting at `data`, returning (type, value_slice,
/// field_start_offset) for each.
fn parse_efs(data: &[u8]) -> Option<Vec<(u16, &[u8], usize)>> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 4 <= data.len() {
        let ty = u16::from_be_bytes([data[p], data[p + 1]]);
        let len = u16::from_be_bytes([data[p + 2], data[p + 3]]) as usize;
        if len < 4 || p + len > data.len() {
            return None;
        }
        out.push((ty, &data[p + 4..p + len], p));
        p += len;
    }
    Some(out)
}

/// Build the Authenticator EF (0x0404) over `aad` (the packet so far), optionally
/// encrypting `plaintext` extension fields.
fn build_auth_ef(key: &[u8; 32], nonce: &[u8; NONCE_LEN], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let ct = cipher.encrypt(Nonce::from_slice(nonce), Payload { msg: plaintext, aad }).expect("aead");
    let mut value = Vec::new();
    value.extend_from_slice(&(NONCE_LEN as u16).to_be_bytes());
    value.extend_from_slice(&(ct.len() as u16).to_be_bytes());
    value.extend_from_slice(nonce);
    let np = pad4(NONCE_LEN) - NONCE_LEN;
    value.resize(value.len() + np, 0);
    value.extend_from_slice(&ct);
    let cp = pad4(ct.len()) - ct.len();
    value.resize(value.len() + cp, 0);
    ef(EF_AUTH, &value)
}

fn ntp_client_header(transmit_ts: u64) -> [u8; NTP_HEADER_LEN] {
    let mut h = [0u8; NTP_HEADER_LEN];
    h[0] = 0x23; // LI=0, VN=4, Mode=3 (client)
    h[40..48].copy_from_slice(&transmit_ts.to_be_bytes()); // Transmit Timestamp
    h
}
fn ntp_server_header(recv_client_ts: u64, transmit_ts: u64) -> [u8; NTP_HEADER_LEN] {
    let mut h = [0u8; NTP_HEADER_LEN];
    h[0] = 0x24; // LI=0, VN=4, Mode=4 (server)
    h[1] = 1; // stratum 1
    h[24..32].copy_from_slice(&recv_client_ts.to_be_bytes()); // Origin = client's transmit
    h[40..48].copy_from_slice(&transmit_ts.to_be_bytes()); // Transmit
    h
}

/// A parsed, authenticated NTS response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Server transmit timestamp (NTP 64-bit: seconds since 1900 << 32 | frac).
    pub transmit_ts: u64,
    /// Fresh NTS cookies for the next request (decrypted from the response).
    pub cookies: Vec<Vec<u8>>,
}
impl Response {
    /// Seconds since the Unix epoch (drops the fractional part).
    pub fn unix_secs(&self) -> u64 {
        (self.transmit_ts >> 32).saturating_sub(2_208_988_800)
    }
}

/// Build an NTS-protected client request: NTP header + Unique Identifier + NTS
/// Cookie + Authenticator (empty plaintext). Returns (packet, unique_id) — keep
/// `unique_id` to match against the reply.
pub fn client_request(
    c2s_key: &[u8; 32],
    cookie: &[u8],
    unique_id: [u8; 32],
    nonce: [u8; NONCE_LEN],
    transmit_ts: u64,
) -> Vec<u8> {
    let mut pkt = Vec::new();
    pkt.extend_from_slice(&ntp_client_header(transmit_ts));
    pkt.extend_from_slice(&ef(EF_UNIQUE_ID, &unique_id));
    pkt.extend_from_slice(&ef(EF_COOKIE, cookie));
    let auth = build_auth_ef(c2s_key, &nonce, &pkt, &[]);
    pkt.extend_from_slice(&auth);
    pkt
}

/// Build an NTS-protected server response: echoes the Unique Identifier and
/// encrypts fresh cookies inside the Authenticator EF.
pub fn server_response(
    s2c_key: &[u8; 32],
    unique_id: &[u8],
    new_cookies: &[&[u8]],
    nonce: [u8; NONCE_LEN],
    recv_client_ts: u64,
    transmit_ts: u64,
) -> Vec<u8> {
    let mut pkt = Vec::new();
    pkt.extend_from_slice(&ntp_server_header(recv_client_ts, transmit_ts));
    pkt.extend_from_slice(&ef(EF_UNIQUE_ID, unique_id));
    // Encrypted plaintext = the new NTS Cookie EFs.
    let mut plaintext = Vec::new();
    for c in new_cookies {
        plaintext.extend_from_slice(&ef(EF_COOKIE, c));
    }
    let auth = build_auth_ef(s2c_key, &nonce, &pkt, &plaintext);
    pkt.extend_from_slice(&auth);
    pkt
}

/// Verify an NTS response against the S2C key and the expected Unique Identifier:
/// checks the identifier (anti off-path) and the AEAD authenticator (anti
/// forgery/tamper). Only on success is the server time returned as authentic.
pub fn verify_response(s2c_key: &[u8; 32], expected_unique_id: &[u8; 32], packet: &[u8]) -> Result<Response, NtsError> {
    if packet.len() < NTP_HEADER_LEN {
        return Err(NtsError::Malformed);
    }
    let header = &packet[..NTP_HEADER_LEN];
    let efs = parse_efs(&packet[NTP_HEADER_LEN..]).ok_or(NtsError::Malformed)?;

    // Unique Identifier must echo the request's.
    let uid = efs.iter().find(|(t, _, _)| *t == EF_UNIQUE_ID).map(|(_, v, _)| *v).ok_or(NtsError::Malformed)?;
    if uid != expected_unique_id {
        return Err(NtsError::UniqueIdMismatch);
    }

    // Locate the Authenticator EF; AAD = everything before it.
    let (_, auth_val, auth_off) =
        *efs.iter().find(|(t, _, _)| *t == EF_AUTH).ok_or(NtsError::MissingAuth)?;
    let aad_end = NTP_HEADER_LEN + auth_off;
    let aad = &packet[..aad_end];

    if auth_val.len() < 4 {
        return Err(NtsError::Malformed);
    }
    let nonce_len = u16::from_be_bytes([auth_val[0], auth_val[1]]) as usize;
    let ct_len = u16::from_be_bytes([auth_val[2], auth_val[3]]) as usize;
    let np = pad4(nonce_len);
    let nonce = auth_val.get(4..4 + nonce_len).ok_or(NtsError::Malformed)?;
    let ct = auth_val.get(4 + np..4 + np + ct_len).ok_or(NtsError::Malformed)?;
    if nonce.len() != NONCE_LEN {
        return Err(NtsError::Malformed);
    }

    let cipher = ChaCha20Poly1305::new(Key::from_slice(s2c_key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|_| NtsError::AuthFailed)?;

    // Decrypted plaintext holds the fresh cookies.
    let mut cookies = Vec::new();
    if let Some(inner) = parse_efs(&plaintext) {
        for (t, v, _) in inner {
            if t == EF_COOKIE {
                cookies.push(v.to_vec());
            }
        }
    }
    let transmit_ts = u64::from_be_bytes(header[40..48].try_into().unwrap());
    Ok(Response { transmit_ts, cookies })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> ([u8; 32], [u8; 32]) {
        // Both sides derive from the same TLS exporter secret → matching keys.
        let ems = [0x42u8; 32];
        (derive_key(&ems, AEAD_CHACHA20_POLY1305, true), derive_key(&ems, AEAD_CHACHA20_POLY1305, false))
    }

    #[test]
    fn c2s_and_s2c_keys_differ_and_are_deterministic() {
        let (c2s, s2c) = keys();
        assert_ne!(c2s, s2c);
        let (c2s2, _) = keys();
        assert_eq!(c2s, c2s2); // deterministic given the exporter secret
    }

    #[test]
    fn authenticated_exchange_roundtrip() {
        let (c2s, s2c) = keys();
        let uid = [7u8; 32];
        let _req = client_request(&c2s, b"cookie-0", uid, [1u8; 12], 0xAABB_CCDD_0000_0000);
        // Server replies with fresh cookies + its time.
        let server_time = 0xE9F0_1234_5678_9ABCu64;
        let resp = server_response(&s2c, &uid, &[b"cookie-1", b"cookie-2"], [2u8; 12], 0, server_time);
        let out = verify_response(&s2c, &uid, &resp).unwrap();
        assert_eq!(out.transmit_ts, server_time);
        assert_eq!(out.cookies, alloc::vec![b"cookie-1".to_vec(), b"cookie-2".to_vec()]);
    }

    #[test]
    fn tampered_response_is_rejected() {
        let (_c2s, s2c) = keys();
        let uid = [9u8; 32];
        let mut resp = server_response(&s2c, &uid, &[b"cookie-1"], [3u8; 12], 0, 0x1234_5678_0000_0000);
        // Flip a byte in the NTP header (the transmit timestamp) — AEAD covers it.
        resp[44] ^= 0xFF;
        assert_eq!(verify_response(&s2c, &uid, &resp), Err(NtsError::AuthFailed));
    }

    #[test]
    fn off_path_wrong_unique_id_is_rejected() {
        let (_c2s, s2c) = keys();
        let resp = server_response(&s2c, &[0xAAu8; 32], &[b"c"], [4u8; 12], 0, 1);
        // Client was expecting a different Unique Identifier → reject before trusting time.
        assert_eq!(verify_response(&s2c, &[0xBBu8; 32], &resp), Err(NtsError::UniqueIdMismatch));
    }

    #[test]
    fn wrong_key_is_rejected() {
        let (_c2s, s2c) = keys();
        let uid = [1u8; 32];
        let resp = server_response(&s2c, &uid, &[b"c"], [5u8; 12], 0, 1);
        let wrong = [0u8; 32];
        assert_eq!(verify_response(&wrong, &uid, &resp), Err(NtsError::AuthFailed));
    }
}
