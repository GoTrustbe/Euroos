//! RDP licensing (MS-RDPELE), client side, from scratch and `no_std`.
//!
//! After the Client Info PDU the server either answers STATUS_VALID_CLIENT or
//! starts a licensing exchange: SERVER_LICENSE_REQUEST (server random, product
//! info, its RSA certificate) → CLIENT_NEW_LICENSE_REQUEST (client random,
//! RSA-encrypted pre-master secret, user + machine name) →
//! SERVER_PLATFORM_CHALLENGE (RC4-encrypted challenge) →
//! CLIENT_PLATFORM_CHALLENGE_RESPONSE (RC4-encrypted response + HWID, MAC) →
//! SERVER_NEW_LICENSE (or STATUS_VALID_CLIENT). The keys come from MD5/SHA-1
//! salted hashes of the two randoms and the pre-master secret; the licence
//! itself is not stored (a new-license request is made per session, which
//! every server accepts for the grace period; xrdp always).
//!
//! Crypto here is the licensing crypto the protocol prescribes (MD5, SHA-1,
//! RC4, raw RSA); the transport is TLS underneath, so none of it protects
//! anything on the wire, it only satisfies the exchange.

use alloc::vec::Vec;

// ---------- MD5 (RFC 1321) ----------
pub fn md5(msg: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [7,12,17,22,7,12,17,22,7,12,17,22,7,12,17,22,5,9,14,20,5,9,14,20,5,9,14,20,5,9,14,20,
        4,11,16,23,4,11,16,23,4,11,16,23,4,11,16,23,6,10,15,21,6,10,15,21,6,10,15,21,6,10,15,21];
    let mut k = [0u32; 64];
    for (i, kk) in k.iter_mut().enumerate() {
        *kk = (libm_sin(i as f64 + 1.0).abs() * 4294967296.0) as u32;
    }
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut m = msg.to_vec();
    let bitlen = (msg.len() as u64).wrapping_mul(8);
    m.push(0x80);
    while m.len() % 64 != 56 { m.push(0); }
    m.extend_from_slice(&bitlen.to_le_bytes());
    for chunk in m.chunks(64) {
        let mut w = [0u32; 16];
        for i in 0..16 { w[i] = u32::from_le_bytes([chunk[4*i], chunk[4*i+1], chunk[4*i+2], chunk[4*i+3]]); }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(w[g]);
            a = d; d = c; c = b;
            b = b.wrapping_add(f2.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a); b0 = b0.wrapping_add(b); c0 = c0.wrapping_add(c); d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes()); out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes()); out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// sin(x) for the MD5 constant table, without libm: Taylor series after range
/// reduction (|x| ≤ 64 here, and we need ~10 significant digits, which is what
/// the floor(|sin| · 2^32) table needs; verified against the RFC vectors).
fn libm_sin(x: f64) -> f64 {
    let two_pi = 6.283185307179586;
    let mut r = x % two_pi;
    if r > 3.141592653589793 { r -= two_pi; }
    if r < -3.141592653589793 { r += two_pi; }
    let (mut term, mut sum, x2) = (r, r, r * r);
    for i in 1..20 {
        term *= -x2 / ((2 * i) as f64 * (2 * i + 1) as f64);
        sum += term;
    }
    sum
}

// ---------- SHA-1 (FIPS 180-4) ----------
pub fn sha1(msg: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut m = msg.to_vec();
    let bitlen = (msg.len() as u64).wrapping_mul(8);
    m.push(0x80);
    while m.len() % 64 != 56 { m.push(0); }
    m.extend_from_slice(&bitlen.to_be_bytes());
    for chunk in m.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 { w[i] = u32::from_be_bytes([chunk[4*i], chunk[4*i+1], chunk[4*i+2], chunk[4*i+3]]); }
        for i in 16..80 { w[i] = (w[i-3] ^ w[i-8] ^ w[i-14] ^ w[i-16]).rotate_left(1); }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for i in 0..80 {
            let (f, k) = match i / 20 {
                0 => ((b & c) | (!b & d), 0x5A827999u32),
                1 => (b ^ c ^ d, 0x6ED9EBA1),
                2 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(w[i]);
            e = d; d = c; c = b.rotate_left(30); b = a; a = t;
        }
        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b); h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d); h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for i in 0..5 { out[4*i..4*i+4].copy_from_slice(&h[i].to_be_bytes()); }
    out
}

// ---------- RC4 ----------
pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s = [0u8; 256];
    for (i, v) in s.iter_mut().enumerate() { *v = i as u8; }
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter().map(|&b| {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        b ^ s[(s[i as usize].wrapping_add(s[j as usize])) as usize]
    }).collect()
}

// ---------- licensing key schedule (MS-RDPELE 5.1.3) ----------
fn salted_hash(secret: &[u8], r1: &[u8; 32], r2: &[u8; 32]) -> [u8; 48] {
    let mut out = [0u8; 48];
    for (i, salt) in [&b"A"[..], &b"BB"[..], &b"CCC"[..]].iter().enumerate() {
        let mut inner = Vec::new();
        inner.extend_from_slice(salt); inner.extend_from_slice(secret);
        inner.extend_from_slice(r1); inner.extend_from_slice(r2);
        let sh = sha1(&inner);
        let mut outer = Vec::new();
        outer.extend_from_slice(secret); outer.extend_from_slice(&sh);
        out[16*i..16*i+16].copy_from_slice(&md5(&outer));
    }
    out
}

/// The three session keys from the pre-master secret and the two randoms.
pub struct Keys {
    pub mac_salt: [u8; 16],
    pub enc: [u8; 16],
}

pub fn derive_keys(premaster: &[u8; 48], client_random: &[u8; 32], server_random: &[u8; 32]) -> Keys {
    let master = salted_hash(premaster, client_random, server_random);
    let blob = salted_hash(&master, server_random, client_random);
    let mut mac_salt = [0u8; 16];
    mac_salt.copy_from_slice(&blob[0..16]);
    let mut fh = Vec::new();
    fh.extend_from_slice(&blob[16..32]); fh.extend_from_slice(client_random); fh.extend_from_slice(server_random);
    Keys { mac_salt, enc: md5(&fh) }
}

/// MACData over `data` (MS-RDPELE 2.2.2.6 / the SSL-3 style MAC): SHA-1(salt |
/// pad1 | len | data) then MD5(salt | pad2 | sha1).
pub fn mac_data(mac_salt: &[u8; 16], data: &[u8]) -> [u8; 16] {
    let mut inner = Vec::new();
    inner.extend_from_slice(mac_salt);
    inner.extend_from_slice(&[0x36u8; 40]);
    inner.extend_from_slice(&(data.len() as u32).to_le_bytes());
    inner.extend_from_slice(data);
    let sh = sha1(&inner);
    let mut outer = Vec::new();
    outer.extend_from_slice(mac_salt);
    outer.extend_from_slice(&[0x5Cu8; 48]);
    outer.extend_from_slice(&sh);
    md5(&outer)
}

// ---------- RSA (raw, little-endian as the protocol wants) ----------
/// Raw RSA public operation of `m` (little-endian) with the server's modulus and
/// exponent (both little-endian, as in the proprietary certificate); the result
/// is little-endian and `modulus.len()` long. No padding: that is what the
/// protocol specifies for the pre-master secret.
pub fn rsa_public_le(modulus_le: &[u8], exponent_le: &[u8], m_le: &[u8]) -> Option<Vec<u8>> {
    use crypto_bigint::modular::runtime_mod::{DynResidue, DynResidueParams};
    use crypto_bigint::{Encoding, U4096};
    let k = modulus_le.len();
    if k == 0 || k > 512 || m_le.len() > k { return None; }
    let to_u = |le: &[u8]| -> Option<U4096> {
        if le.len() > 512 { return None; }
        let mut be = [0u8; 512];
        for (i, b) in le.iter().enumerate() { be[511 - i] = *b; }
        Some(U4096::from_be_slice(&be))
    };
    let n = to_u(modulus_le)?;
    if modulus_le.iter().all(|&b| b == 0) || modulus_le[0] & 1 == 0 { return None; } // zero or even modulus: not RSA
    let e = to_u(exponent_le)?;
    let m = to_u(m_le)?;
    let params = DynResidueParams::new(&n);
    let c = DynResidue::new(&m, params).pow(&e).retrieve().to_be_bytes();
    let mut out = alloc::vec![0u8; k];
    for i in 0..k { out[i] = c[511 - i]; }
    Some(out)
}

// ---------- messages ----------
pub const MSG_LICENSE_REQUEST: u8 = 0x01;
pub const MSG_PLATFORM_CHALLENGE: u8 = 0x02;
pub const MSG_NEW_LICENSE: u8 = 0x03;
pub const MSG_UPGRADE_LICENSE: u8 = 0x04;
pub const MSG_NEW_LICENSE_REQUEST: u8 = 0x13;
pub const MSG_PLATFORM_CHALLENGE_RESPONSE: u8 = 0x15;
pub const MSG_ERROR_ALERT: u8 = 0xFF;
const PREAMBLE_V3: u8 = 0x03;
const BB_DATA_BLOB: u16 = 0x0009;
const BB_RANDOM_BLOB: u16 = 0x0002;
const BB_CLIENT_USER_NAME_BLOB: u16 = 0x000F;
const BB_CLIENT_MACHINE_NAME_BLOB: u16 = 0x0010;

fn u16le(b: &[u8], o: usize) -> Option<u16> { Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?])) }
fn u32le(b: &[u8], o: usize) -> Option<u32> { Some(u32::from_le_bytes([*b.get(o)?, *b.get(o+1)?, *b.get(o+2)?, *b.get(o+3)?])) }

/// A BLOB: wBlobType(2) wBlobLen(2) data. Returns (type, data, bytes consumed).
fn read_blob(b: &[u8], o: usize) -> Option<(u16, &[u8], usize)> {
    let t = u16le(b, o)?;
    let len = u16le(b, o + 2)? as usize;
    let data = b.get(o + 4..o + 4 + len)?;
    Some((t, data, 4 + len))
}

fn put_blob(v: &mut Vec<u8>, t: u16, data: &[u8]) {
    v.extend_from_slice(&t.to_le_bytes());
    v.extend_from_slice(&(data.len() as u16).to_le_bytes());
    v.extend_from_slice(data);
}

/// The server's RSA public key, little-endian modulus and exponent, as carried
/// in a PROPRIETARY certificate (CERT_CHAIN_VERSION_1, what xrdp sends).
pub struct ServerKey {
    pub modulus_le: Vec<u8>,
    pub exponent_le: Vec<u8>,
}

/// What a SERVER_LICENSE_REQUEST gives us.
pub struct LicenseRequest {
    pub server_random: [u8; 32],
    pub key: Option<ServerKey>,
}

/// Parse the licensing-message body (after the 4-byte preamble) of a
/// SERVER_LICENSE_REQUEST.
pub fn parse_license_request(body: &[u8]) -> Option<LicenseRequest> {
    let mut sr = [0u8; 32];
    sr.copy_from_slice(body.get(0..32)?);
    let mut o = 32;
    // ProductInfo: dwVersion, cbCompanyName, CompanyName, cbProductId, ProductId
    let _ver = u32le(body, o)?; o += 4;
    let cb_company = u32le(body, o)? as usize; o += 4 + cb_company;
    let cb_product = u32le(body, o)? as usize; o += 4 + cb_product;
    // KeyExchangeList blob
    let (_, _kex, n) = read_blob(body, o)?; o += n;
    // ServerCertificate blob
    let (_, cert, n) = read_blob(body, o)?; o += n;
    let _ = o; // ScopeList follows; not needed
    Some(LicenseRequest { server_random: sr, key: parse_server_cert(cert) })
}

/// The server certificate blob: CERT_CHAIN_VERSION_1 (proprietary, xrdp) or
/// CERT_CHAIN_VERSION_2 (an X.509 chain, Windows): the RSA key of the LAST
/// certificate in the chain is the licensing key.
pub fn parse_server_cert(cert: &[u8]) -> Option<ServerKey> {
    let version = u32le(cert, 0)? & 0x7FFF_FFFF;
    match version {
        1 => parse_proprietary_cert(cert),
        2 => parse_x509_chain(cert),
        _ => None,
    }
}

/// X509_CERTIFICATE_CHAIN (MS-RDPBCGR 2.2.1.4.3.2): dwVersion(4), NumCertBlobs(4),
/// then NumCertBlobs × { cbCert(4), abCert }. The leaf is the last one; its
/// SubjectPublicKeyInfo RSA modulus/exponent (big-endian DER) become the
/// little-endian key the licensing RSA wants.
pub fn parse_x509_chain(cert: &[u8]) -> Option<ServerKey> {
    let n = u32le(cert, 4)? as usize;
    if n == 0 || n > 16 { return None; }
    let mut o = 8;
    let mut last: Option<&[u8]> = None;
    for _ in 0..n {
        let cb = u32le(cert, o)? as usize; o += 4;
        last = Some(cert.get(o..o + cb)?); o += cb;
    }
    let der = last?;
    let c = eurotls::x509::Certificate::parse(der).ok()?;
    let (n_be, e_be) = eurotls::x509::parse_rsa_public_key(c.pubkey).ok()?;
    let strip = |b: &[u8]| -> Vec<u8> { let mut i = 0; while i + 1 < b.len() && b[i] == 0 { i += 1; } b[i..].to_vec() };
    let (mut n_le, mut e_le) = (strip(n_be), strip(e_be));
    n_le.reverse(); e_le.reverse();
    if n_le.len() < 64 || n_le.len() > 512 { return None; }
    Some(ServerKey { modulus_le: n_le, exponent_le: e_le })
}

/// PROPRIETARYSERVERCERTIFICATE (MS-RDPBCGR 2.2.1.4.3.1.1): dwVersion(4, low
/// bits 1), dwSigAlgId(4), dwKeyAlgId(4), wPublicKeyBlobType(2)=0x0006,
/// wPublicKeyBlobLen(2), RSA_PUBLIC_KEY { magic "RSA1", keylen, bitlen, datalen,
/// pubExp(4), modulus(keylen bytes, little-endian, incl. 8 padding bytes) }.
pub fn parse_proprietary_cert(cert: &[u8]) -> Option<ServerKey> {
    let version = u32le(cert, 0)?;
    if version & 0x7FFF_FFFF != 1 { return None; } // X.509 chain (version 2): next rung
    let mut o = 12;
    let bt = u16le(cert, o)?; o += 2;
    if bt != 0x0006 { return None; }
    let blen = u16le(cert, o)? as usize; o += 2;
    let blob = cert.get(o..o + blen)?;
    if blob.get(0..4)? != b"RSA1" { return None; }
    let keylen = u32le(blob, 4)? as usize;
    let _bitlen = u32le(blob, 8)?;
    let datalen = u32le(blob, 12)? as usize; // = keylen - 8: the usable modulus bytes
    let exp = blob.get(16..20)?.to_vec();
    let modulus = blob.get(20..20 + keylen)?;
    let used = if datalen > 0 && datalen <= keylen { datalen } else { keylen };
    Some(ServerKey { modulus_le: modulus[..used].to_vec(), exponent_le: exp })
}

fn preamble(msg: u8, body_len: usize) -> [u8; 4] {
    let size = (4 + body_len) as u16;
    [msg, PREAMBLE_V3, size as u8, (size >> 8) as u8]
}

/// CLIENT_NEW_LICENSE_REQUEST (2.2.2.2). `premaster` is encrypted with the
/// server key (raw RSA, little-endian, plus 8 zero bytes as the protocol pads).
/// Returns the whole licensing message (preamble + body), or None when the
/// server key is unusable.
pub fn new_license_request(key: &ServerKey, client_random: &[u8; 32], premaster: &[u8; 48], user: &str, machine: &str) -> Option<Vec<u8>> {
    let enc = rsa_public_le(&key.modulus_le, &key.exponent_le, premaster)?;
    let mut enc_blob = enc;
    enc_blob.extend_from_slice(&[0u8; 8]);
    let mut body = Vec::new();
    body.extend_from_slice(&1u32.to_le_bytes()); // KEY_EXCHANGE_ALG_RSA
    body.extend_from_slice(&0x0401_0000u32.to_le_bytes()); // CLIENT_OS_ID_WINNT_POST_52 | CLIENT_IMAGE_ID_MICROSOFT
    body.extend_from_slice(client_random);
    put_blob(&mut body, BB_RANDOM_BLOB, &enc_blob);
    let mut u = user.as_bytes().to_vec(); u.push(0);
    put_blob(&mut body, BB_CLIENT_USER_NAME_BLOB, &u);
    let mut m = machine.as_bytes().to_vec(); m.push(0);
    put_blob(&mut body, BB_CLIENT_MACHINE_NAME_BLOB, &m);
    let mut v = preamble(MSG_NEW_LICENSE_REQUEST, body.len()).to_vec();
    v.extend_from_slice(&body);
    Some(v)
}

/// SERVER_PLATFORM_CHALLENGE body: ConnectFlags(4), EncryptedPlatformChallenge
/// BLOB, MACData(16). Returns the decrypted challenge.
pub fn parse_platform_challenge(body: &[u8], keys: &Keys) -> Option<Vec<u8>> {
    let (_, enc, _) = read_blob(body, 4)?;
    Some(rc4(&keys.enc, enc))
}

/// CLIENT_PLATFORM_CHALLENGE_RESPONSE (2.2.2.5): the challenge echoed in a
/// PLATFORM_CHALLENGE_RESPONSE_DATA, plus a CLIENT_HARDWARE_ID, both RC4-encrypted,
/// and the MAC over the two plaintexts.
pub fn platform_challenge_response(challenge: &[u8], keys: &Keys, hwid_seed: &[u8; 16]) -> Vec<u8> {
    let mut resp = Vec::new();
    resp.extend_from_slice(&0x0100u16.to_le_bytes()); // wVersion
    resp.extend_from_slice(&0x0100u16.to_le_bytes()); // wClientType (OTHER)
    resp.extend_from_slice(&0x0003u16.to_le_bytes()); // wLicenseDetailLevel (DETAIL)
    resp.extend_from_slice(&(challenge.len() as u16).to_le_bytes());
    resp.extend_from_slice(challenge);
    let mut hwid = Vec::new();
    hwid.extend_from_slice(&2u32.to_le_bytes()); // PlatformId
    hwid.extend_from_slice(hwid_seed);
    let mut mac_in = resp.clone();
    mac_in.extend_from_slice(&hwid);
    let mac = mac_data(&keys.mac_salt, &mac_in);
    let mut body = Vec::new();
    put_blob(&mut body, BB_DATA_BLOB, &rc4(&keys.enc, &resp));
    put_blob(&mut body, BB_DATA_BLOB, &rc4(&keys.enc, &hwid));
    body.extend_from_slice(&mac);
    let mut v = preamble(MSG_PLATFORM_CHALLENGE_RESPONSE, body.len()).to_vec();
    v.extend_from_slice(&body);
    v
}

/// Split a licensing message into (bMsgType, body). The preamble is 4 bytes.
pub fn split(msg: &[u8]) -> Option<(u8, &[u8])> {
    let t = *msg.first()?;
    let size = u16le(msg, 2)? as usize;
    let end = size.min(msg.len()).max(4);
    Some((t, &msg[4..end]))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hex(b: &[u8]) -> alloc::string::String {
        use core::fmt::Write;
        let mut s = alloc::string::String::new();
        for x in b { let _ = write!(s, "{x:02x}"); }
        s
    }
    #[test]
    fn md5_rfc1321_vectors() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hex(&md5(b"The quick brown fox jumps over the lazy dog")), "9e107d9d372bb6826bd81d3542a419d6");
    }
    #[test]
    fn sha1_fips_vectors() {
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }
    #[test]
    fn rc4_known_vector() {
        // Wikipedia/RFC 6229 style: key "Key", plaintext "Plaintext"
        assert_eq!(hex(&rc4(b"Key", b"Plaintext")), "bbf316e8d940af0ad3");
        let c = rc4(b"Secret", b"Attack at dawn");
        assert_eq!(rc4(b"Secret", &c), b"Attack at dawn");
    }
    #[test]
    fn keys_are_deterministic_and_distinct() {
        let pm = [7u8; 48]; let cr = [1u8; 32]; let sr = [2u8; 32];
        let k1 = derive_keys(&pm, &cr, &sr);
        let k2 = derive_keys(&pm, &cr, &sr);
        assert_eq!(k1.enc, k2.enc);
        assert_ne!(k1.enc, k1.mac_salt);
        let k3 = derive_keys(&pm, &sr, &cr);
        assert_ne!(k1.enc, k3.enc);
    }
    #[test]
    fn rsa_raw_roundtrip_small_key() {
        // n = 3233 = 61*97, e = 17, d = 2753 (textbook). m = 65.
        let n_le = [0xA1u8, 0x0C]; // 3233 = 0x0CA1
        let e_le = [17u8];
        let m_le = [65u8, 0];
        let c = rsa_public_le(&n_le, &e_le, &m_le).unwrap();
        // 65^17 mod 3233 = 2790
        assert_eq!(u16::from_le_bytes([c[0], c[1]]), 2790);
        let d_le = [0xC1u8, 0x0A]; // 2753
        let back = rsa_public_le(&n_le, &d_le, &c).unwrap();
        assert_eq!(u16::from_le_bytes([back[0], back[1]]), 65);
    }
    fn fake_license_request(modulus_le: &[u8], exp_le: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&[0xAAu8; 32]); // server random
        body.extend_from_slice(&0x0002_0000u32.to_le_bytes()); // product version
        body.extend_from_slice(&4u32.to_le_bytes()); body.extend_from_slice(b"X\0\0\0");
        body.extend_from_slice(&2u32.to_le_bytes()); body.extend_from_slice(b"A\0");
        put_blob(&mut body, 0x000D, &1u32.to_le_bytes()); // key exchange list
        // proprietary cert
        let mut cert = Vec::new();
        cert.extend_from_slice(&1u32.to_le_bytes());
        cert.extend_from_slice(&1u32.to_le_bytes());
        cert.extend_from_slice(&1u32.to_le_bytes());
        let mut rsa = Vec::new();
        rsa.extend_from_slice(b"RSA1");
        let keylen = modulus_le.len() + 8;
        rsa.extend_from_slice(&(keylen as u32).to_le_bytes());
        rsa.extend_from_slice(&((modulus_le.len() * 8) as u32).to_le_bytes());
        rsa.extend_from_slice(&(modulus_le.len() as u32).to_le_bytes());
        let mut e4 = [0u8; 4]; e4[..exp_le.len()].copy_from_slice(exp_le);
        rsa.extend_from_slice(&e4);
        rsa.extend_from_slice(modulus_le); rsa.extend_from_slice(&[0u8; 8]);
        cert.extend_from_slice(&0x0006u16.to_le_bytes());
        cert.extend_from_slice(&(rsa.len() as u16).to_le_bytes());
        cert.extend_from_slice(&rsa);
        cert.extend_from_slice(&0x0008u16.to_le_bytes()); cert.extend_from_slice(&0u16.to_le_bytes());
        put_blob(&mut body, 0x0003, &cert);
        body.extend_from_slice(&0u32.to_le_bytes()); // scope count
        let mut v = preamble(MSG_LICENSE_REQUEST, body.len()).to_vec();
        v.extend_from_slice(&body);
        v
    }
    #[test]
    fn license_request_parse_and_new_license_request() {
        let msg = fake_license_request(&[0xA1, 0x0C], &[17]);
        let (t, body) = split(&msg).unwrap();
        assert_eq!(t, MSG_LICENSE_REQUEST);
        let req = parse_license_request(body).unwrap();
        assert_eq!(req.server_random, [0xAA; 32]);
        let key = req.key.unwrap();
        assert_eq!(key.modulus_le, [0xA1, 0x0C]);
        // a 48-byte premaster does not fit a 2-byte modulus: refused, not garbage
        assert!(new_license_request(&key, &[1; 32], &[9; 48], "euro", "euroos").is_none());
    }
    #[test]
    fn x509_chain_key_and_rsa_against_openssl_private_key() {
        // A self-signed RSA-1024 certificate made with openssl (testdata); its
        // private exponent d is embedded so the raw public operation can be
        // inverted here: m^e mod n then (.)^d mod n must give m back.
        const DER: &[u8] = include_bytes!("../testdata/rsa1024-selfsigned.der");
        const D_HEX: &str = "0b77372acbde44bf79b3de8fc3bb5a03545dcd4944bed8252c4411747e59e994690ead8e086e4472ad3bc63c951240bb92c05f7f5f2eb50a5878ae5e60a5dd09feae1f50b24278e8795431f99325320ba0a61810c4c79d37562650580908ea146f8e46405636343d4d3416d4cbc8d667093d4af8bb0e8f4a60aea110e7835c3d";
        const N_HEX: &str = "a66dce8ef142ff5c9dd0ba2e82b3283d24dcc5093e6ed792eec6e32bee72308dac2557de51c73c8d6436dafec522dbfc578aa53581ad581d57c5dfa0b03ca6b229f0d0390b1e68dcff0653a91461f27c06552dc0da50654365d2b0d7f5bc135658b7d32b950123899a4a2714f955fd7d52d58569ee53568f479d3f60bab2e0e1";
        let mut chain = Vec::new();
        chain.extend_from_slice(&2u32.to_le_bytes());
        chain.extend_from_slice(&1u32.to_le_bytes());
        chain.extend_from_slice(&(DER.len() as u32).to_le_bytes());
        chain.extend_from_slice(DER);
        let key = parse_server_cert(&chain).expect("x509 chain parses");
        assert_eq!(key.modulus_le.len(), 128);
        assert_eq!(key.exponent_le, alloc::vec![0x01, 0x00, 0x01]);
        let unhex = |h: &str| -> Vec<u8> { (0..h.len()/2).map(|i| u8::from_str_radix(&h[2*i..2*i+2], 16).unwrap()).collect() };
        let mut n_le = unhex(N_HEX); n_le.reverse();
        assert_eq!(n_le, key.modulus_le);
        let mut d_le = unhex(D_HEX); d_le.reverse();
        let premaster = [0x5Au8; 48];
        let c = rsa_public_le(&key.modulus_le, &key.exponent_le, &premaster).unwrap();
        assert_eq!(c.len(), 128);
        let back = rsa_public_le(&key.modulus_le, &d_le, &c).unwrap();
        assert_eq!(&back[..48], &premaster[..]);
        assert!(back[48..].iter().all(|&b| b == 0));
        assert!(new_license_request(&key, &[1; 32], &premaster, "euro", "euroos").is_some());
    }

    #[test]
    fn challenge_response_shape() {
        let keys = derive_keys(&[3; 48], &[1; 32], &[2; 32]);
        let chal = alloc::vec![0x54, 0x00, 0x45, 0x00, 0x53, 0x00, 0x54, 0x00, 0x00, 0x00];
        let enc = rc4(&keys.enc, &chal);
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_le_bytes());
        put_blob(&mut body, BB_DATA_BLOB, &enc);
        body.extend_from_slice(&[0u8; 16]);
        assert_eq!(parse_platform_challenge(&body, &keys).unwrap(), chal);
        let resp = platform_challenge_response(&chal, &keys, &[5; 16]);
        assert_eq!(resp[0], MSG_PLATFORM_CHALLENGE_RESPONSE);
        assert_eq!(u16::from_le_bytes([resp[2], resp[3]]) as usize, resp.len());
    }
}
