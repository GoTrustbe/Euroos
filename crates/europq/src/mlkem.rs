//! ML-KEM-768 (FIPS 203) from scratch — the post-quantum KEM for hybrid key
//! exchange. Confidentiality against "harvest now, decrypt later": a quantum
//! adversary who records today's traffic still cannot recover the session key.
//!
//! Verified byte-for-byte against the NIST ACVP known-answer vectors
//! (`tests/kat.rs`). Paired with X25519 in a hybrid handshake so security holds
//! if EITHER primitive stands (see [`crate::hybrid`]).

use alloc::vec;
use alloc::vec::Vec;

use crate::keccak::{sha3_256, sha3_512, shake128_xof, shake256, Sponge};

const Q: i32 = 3329;
const N: usize = 256;
const K: usize = 3; // ML-KEM-768
const ETA: usize = 2; // eta1 = eta2 = 2
const ETA_BYTES: usize = 64 * ETA;
const DU: u32 = 10;
const DV: u32 = 4;
const POLYBYTES: usize = 384; // 12-bit packed poly

/// Public encapsulation key length (bytes).
pub const EK_LEN: usize = 12 * K * 32 + 32; // 1184
const DKPKE_LEN: usize = 12 * K * 32; // 1152
/// Secret decapsulation key length (bytes).
pub const DK_LEN: usize = DKPKE_LEN + EK_LEN + 32 + 32; // 2400
const C1_LEN: usize = (DU as usize) * K * 32; // 960
const C2_LEN: usize = (DV as usize) * 32; // 128
/// Ciphertext length (bytes).
pub const CT_LEN: usize = C1_LEN + C2_LEN; // 1088
/// Shared-secret length (bytes).
pub const SS_LEN: usize = 32;

type Poly = [i16; N];

// ── modular helpers (values kept in [0, Q)) ───────────────────────────────
#[inline]
fn fqadd(a: i16, b: i16) -> i16 {
    let s = a as i32 + b as i32;
    (if s >= Q { s - Q } else { s }) as i16
}
#[inline]
fn fqsub(a: i16, b: i16) -> i16 {
    let s = a as i32 - b as i32;
    (if s < 0 { s + Q } else { s }) as i16
}
#[inline]
fn fqmul(a: i16, b: i16) -> i16 {
    (a as i32 * b as i32).rem_euclid(Q) as i16
}

const fn modpow(mut base: i64, mut exp: u64, m: i64) -> i64 {
    let mut r = 1i64;
    base %= m;
    while exp > 0 {
        if exp & 1 == 1 {
            r = r * base % m;
        }
        base = base * base % m;
        exp >>= 1;
    }
    r
}
const fn brv7(mut x: u32) -> u32 {
    let mut r = 0u32;
    let mut i = 0;
    while i < 7 {
        r = (r << 1) | (x & 1);
        x >>= 1;
        i += 1;
    }
    r
}
const fn gen_zetas() -> [i16; 128] {
    let mut z = [0i16; 128];
    let mut i = 0;
    while i < 128 {
        z[i] = modpow(17, brv7(i as u32) as u64, Q as i64) as i16;
        i += 1;
    }
    z
}
const ZETAS: [i16; 128] = gen_zetas();
const NINV: i16 = modpow(128, (Q - 2) as u64, Q as i64) as i16; // 128^{-1} mod q

// ── NTT / inverse NTT (Cooley-Tukey / Gentleman-Sande, plain arithmetic) ───
fn ntt(r: &mut Poly) {
    let mut k = 1usize;
    let mut len = 128usize;
    while len >= 2 {
        let mut start = 0usize;
        while start < N {
            let zeta = ZETAS[k];
            k += 1;
            let mut j = start;
            while j < start + len {
                let t = fqmul(zeta, r[j + len]);
                r[j + len] = fqsub(r[j], t);
                r[j] = fqadd(r[j], t);
                j += 1;
            }
            start += 2 * len;
        }
        len >>= 1;
    }
}
fn invntt(r: &mut Poly) {
    let mut k = 127usize;
    let mut len = 2usize;
    while len <= 128 {
        let mut start = 0usize;
        while start < N {
            let zeta = ZETAS[k];
            k = k.wrapping_sub(1);
            let mut j = start;
            while j < start + len {
                let t = r[j];
                r[j] = fqadd(t, r[j + len]);
                r[j + len] = fqmul(zeta, fqsub(r[j + len], t));
                j += 1;
            }
            start += 2 * len;
        }
        len <<= 1;
    }
    for c in r.iter_mut() {
        *c = fqmul(*c, NINV);
    }
}

/// Base multiplication of two NTT-domain polynomials (128 degree-1 products).
fn basemul(a: &Poly, b: &Poly) -> Poly {
    let mut r = [0i16; N];
    for i in 0..64 {
        let zeta = ZETAS[64 + i];
        base_case(&mut r, a, b, 4 * i, zeta);
        base_case(&mut r, a, b, 4 * i + 2, fqsub(0, zeta));
    }
    r
}
fn base_case(r: &mut Poly, a: &Poly, b: &Poly, o: usize, zeta: i16) {
    // (a0 + a1 X) * (b0 + b1 X) mod (X^2 - zeta)
    let r0 = fqadd(fqmul(fqmul(a[o + 1], b[o + 1]), zeta), fqmul(a[o], b[o]));
    let r1 = fqadd(fqmul(a[o], b[o + 1]), fqmul(a[o + 1], b[o]));
    r[o] = r0;
    r[o + 1] = r1;
}
fn poly_add(a: &Poly, b: &Poly) -> Poly {
    let mut r = [0i16; N];
    for i in 0..N {
        r[i] = fqadd(a[i], b[i]);
    }
    r
}
fn poly_sub(a: &Poly, b: &Poly) -> Poly {
    let mut r = [0i16; N];
    for i in 0..N {
        r[i] = fqsub(a[i], b[i]);
    }
    r
}

// ── sampling ───────────────────────────────────────────────────────────────
/// SampleNTT: rejection-sample a uniform NTT-domain poly from a SHAKE-128 XOF.
fn sample_ntt(mut xof: Sponge) -> Poly {
    let mut a = [0i16; N];
    let mut j = 0usize;
    let mut buf = [0u8; 3];
    while j < N {
        xof.squeeze(&mut buf);
        let d1 = (buf[0] as u16) | (((buf[1] as u16) & 0x0f) << 8);
        let d2 = ((buf[1] as u16) >> 4) | ((buf[2] as u16) << 4);
        if (d1 as i32) < Q {
            a[j] = d1 as i16;
            j += 1;
        }
        if j < N && (d2 as i32) < Q {
            a[j] = d2 as i16;
            j += 1;
        }
    }
    a
}

/// SamplePolyCBD_eta2: centred binomial noise from `buf` (128 bytes).
fn cbd(buf: &[u8]) -> Poly {
    let bit = |idx: usize| -> i32 { ((buf[idx / 8] >> (idx % 8)) & 1) as i32 };
    let mut f = [0i16; N];
    for (i, c) in f.iter_mut().enumerate() {
        let mut a = 0i32;
        let mut b = 0i32;
        for j in 0..ETA {
            a += bit(2 * ETA * i + j);
        }
        for j in 0..ETA {
            b += bit(2 * ETA * i + ETA + j);
        }
        *c = (a - b).rem_euclid(Q) as i16;
    }
    f
}

/// PRF_eta(s, b) = SHAKE256(s ‖ b) → 64·eta bytes.
fn prf(s: &[u8; 32], b: u8) -> Vec<u8> {
    let mut input = [0u8; 33];
    input[..32].copy_from_slice(s);
    input[32] = b;
    let mut out = vec![0u8; ETA_BYTES];
    shake256(&input, &mut out);
    out
}

/// Generate the matrix Â: `a[i][j] = SampleNTT(SHAKE128(rho ‖ i ‖ j))`.
fn gen_matrix(rho: &[u8]) -> [[Poly; K]; K] {
    let mut a = [[[0i16; N]; K]; K];
    for (i, row) in a.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            // FIPS 203: Â[i,j] ← SampleNTT(XOF(ρ ‖ j ‖ i)) — column index byte
            // first, then row (the transpose convention).
            let mut seed = [0u8; 34];
            seed[..32].copy_from_slice(rho);
            seed[32] = j as u8;
            seed[33] = i as u8;
            *cell = sample_ntt(shake128_xof(&seed));
        }
    }
    a
}

// ── byte (de)serialisation + compression ───────────────────────────────────
fn byte_encode(vals: &[u16], d: usize) -> Vec<u8> {
    let mut out = vec![0u8; vals.len() * d / 8];
    let mut bp = 0usize;
    for &v in vals {
        for i in 0..d {
            if (v >> i) & 1 == 1 {
                out[bp / 8] |= 1 << (bp % 8);
            }
            bp += 1;
        }
    }
    out
}
fn byte_decode(bytes: &[u8], d: usize, n: usize) -> Vec<u16> {
    let mut out = vec![0u16; n];
    let mut bp = 0usize;
    for o in out.iter_mut() {
        let mut v = 0u16;
        for i in 0..d {
            let bit = (bytes[bp / 8] >> (bp % 8)) & 1;
            v |= (bit as u16) << i;
            bp += 1;
        }
        *o = if d == 12 { (v as i32 % Q) as u16 } else { v };
    }
    out
}
#[inline]
fn compress(x: i16, d: u32) -> u16 {
    let x = x as u32;
    let num = (x << d) + (Q as u32 / 2);
    ((num / Q as u32) as u16) & ((1u16 << d) - 1)
}
#[inline]
fn decompress(y: u16, d: u32) -> i16 {
    (((y as u32) * Q as u32 + (1u32 << (d - 1))) >> d) as i16
}

fn poly_to_bytes(p: &Poly) -> Vec<u8> {
    let v: Vec<u16> = p.iter().map(|&c| c as u16).collect();
    byte_encode(&v, 12)
}
fn poly_from_bytes(b: &[u8]) -> Poly {
    let v = byte_decode(b, 12, N);
    let mut p = [0i16; N];
    for (i, &x) in v.iter().enumerate() {
        p[i] = x as i16;
    }
    p
}

// ── K-PKE ──────────────────────────────────────────────────────────────────
fn kpke_keygen(d: &[u8; 32]) -> (Vec<u8>, Vec<u8>) {
    let mut g_in = [0u8; 33];
    g_in[..32].copy_from_slice(d);
    g_in[32] = K as u8;
    let g = sha3_512(&g_in);
    let rho = &g[..32];
    let mut sigma = [0u8; 32];
    sigma.copy_from_slice(&g[32..]);

    let a = gen_matrix(rho);
    let mut nn = 0u8;
    let mut s = [[0i16; N]; K];
    for si in s.iter_mut() {
        *si = cbd(&prf(&sigma, nn));
        nn += 1;
    }
    let mut e = [[0i16; N]; K];
    for ei in e.iter_mut() {
        *ei = cbd(&prf(&sigma, nn));
        nn += 1;
    }
    for si in s.iter_mut() {
        ntt(si);
    }
    for ei in e.iter_mut() {
        ntt(ei);
    }
    // t = A ∘ s + e
    let mut ek = Vec::with_capacity(EK_LEN);
    let mut t = [[0i16; N]; K];
    for i in 0..K {
        let mut acc = [0i16; N];
        for j in 0..K {
            acc = poly_add(&acc, &basemul(&a[i][j], &s[j]));
        }
        t[i] = poly_add(&acc, &e[i]);
    }
    for ti in &t {
        ek.extend_from_slice(&poly_to_bytes(ti));
    }
    ek.extend_from_slice(rho);
    let mut dk = Vec::with_capacity(DKPKE_LEN);
    for si in &s {
        dk.extend_from_slice(&poly_to_bytes(si));
    }
    (ek, dk)
}

fn kpke_encrypt(ek: &[u8], m: &[u8; 32], r: &[u8; 32]) -> Vec<u8> {
    let mut t = [[0i16; N]; K];
    for (i, ti) in t.iter_mut().enumerate() {
        *ti = poly_from_bytes(&ek[i * POLYBYTES..(i + 1) * POLYBYTES]);
    }
    let rho = &ek[K * POLYBYTES..K * POLYBYTES + 32];
    let a = gen_matrix(rho);

    let mut nn = 0u8;
    let mut rr = [[0i16; N]; K];
    for ri in rr.iter_mut() {
        *ri = cbd(&prf(r, nn));
        nn += 1;
    }
    let mut e1 = [[0i16; N]; K];
    for ei in e1.iter_mut() {
        *ei = cbd(&prf(r, nn));
        nn += 1;
    }
    let e2 = cbd(&prf(r, nn));
    for ri in rr.iter_mut() {
        ntt(ri);
    }
    // u = invNTT(A^T ∘ r) + e1
    let mut c = Vec::with_capacity(CT_LEN);
    for i in 0..K {
        let mut acc = [0i16; N];
        for j in 0..K {
            acc = poly_add(&acc, &basemul(&a[j][i], &rr[j]));
        }
        invntt(&mut acc);
        let u = poly_add(&acc, &e1[i]);
        let comp: Vec<u16> = u.iter().map(|&x| compress(x, DU)).collect();
        c.extend_from_slice(&byte_encode(&comp, DU as usize));
    }
    // v = invNTT(t^T ∘ r) + e2 + Decompress1(m)
    let mut acc = [0i16; N];
    for i in 0..K {
        acc = poly_add(&acc, &basemul(&t[i], &rr[i]));
    }
    invntt(&mut acc);
    let mdec = byte_decode(m, 1, N);
    let mut mu = [0i16; N];
    for (i, &b) in mdec.iter().enumerate() {
        mu[i] = decompress(b, 1);
    }
    let v = poly_add(&poly_add(&acc, &e2), &mu);
    let comp: Vec<u16> = v.iter().map(|&x| compress(x, DV)).collect();
    c.extend_from_slice(&byte_encode(&comp, DV as usize));
    c
}

fn kpke_decrypt(dk: &[u8], c: &[u8]) -> [u8; 32] {
    let mut u = [[0i16; N]; K];
    for (i, ui) in u.iter_mut().enumerate() {
        let seg = byte_decode(&c[i * (DU as usize) * 32..(i + 1) * (DU as usize) * 32], DU as usize, N);
        for (j, &x) in seg.iter().enumerate() {
            ui[j] = decompress(x, DU);
        }
    }
    let vseg = byte_decode(&c[C1_LEN..CT_LEN], DV as usize, N);
    let mut v = [0i16; N];
    for (i, &x) in vseg.iter().enumerate() {
        v[i] = decompress(x, DV);
    }
    let mut s = [[0i16; N]; K];
    for (i, si) in s.iter_mut().enumerate() {
        *si = poly_from_bytes(&dk[i * POLYBYTES..(i + 1) * POLYBYTES]);
    }
    // w = v - invNTT(s^T ∘ NTT(u))
    for ui in u.iter_mut() {
        ntt(ui);
    }
    let mut acc = [0i16; N];
    for i in 0..K {
        acc = poly_add(&acc, &basemul(&s[i], &u[i]));
    }
    invntt(&mut acc);
    let w = poly_sub(&v, &acc);
    let comp: Vec<u16> = w.iter().map(|&x| compress(x, 1)).collect();
    let bytes = byte_encode(&comp, 1);
    let mut m = [0u8; 32];
    m.copy_from_slice(&bytes);
    m
}

// ── ML-KEM (FO transform) ──────────────────────────────────────────────────
/// ML-KEM-768 key generation from the 32-byte seeds `d` (K-PKE) and `z`
/// (implicit-rejection secret). Returns (encapsulation key, decapsulation key).
pub fn keygen(d: &[u8; 32], z: &[u8; 32]) -> (Vec<u8>, Vec<u8>) {
    let (ek, dkpke) = kpke_keygen(d);
    let mut dk = Vec::with_capacity(DK_LEN);
    dk.extend_from_slice(&dkpke);
    dk.extend_from_slice(&ek);
    dk.extend_from_slice(&sha3_256(&ek));
    dk.extend_from_slice(z);
    (ek, dk)
}

/// ML-KEM-768 encapsulation with an explicit 32-byte message `m` (the internal
/// randomness). Returns (shared secret, ciphertext).
pub fn encaps_internal(ek: &[u8], m: &[u8; 32]) -> ([u8; 32], Vec<u8>) {
    let mut g_in = Vec::with_capacity(64);
    g_in.extend_from_slice(m);
    g_in.extend_from_slice(&sha3_256(ek));
    let g = sha3_512(&g_in);
    let mut key = [0u8; 32];
    key.copy_from_slice(&g[..32]);
    let mut r = [0u8; 32];
    r.copy_from_slice(&g[32..]);
    let c = kpke_encrypt(ek, m, &r);
    (key, c)
}

/// ML-KEM-768 decapsulation. Always returns a 32-byte secret; on an invalid
/// ciphertext it returns the deterministic implicit-rejection value (never an
/// error), so the failure is indistinguishable to an attacker.
pub fn decaps(dk: &[u8], c: &[u8]) -> [u8; 32] {
    let dkpke = &dk[..DKPKE_LEN];
    let ek = &dk[DKPKE_LEN..DKPKE_LEN + EK_LEN];
    let h = &dk[DKPKE_LEN + EK_LEN..DKPKE_LEN + EK_LEN + 32];
    let z = &dk[DKPKE_LEN + EK_LEN + 32..DK_LEN];

    let m2 = kpke_decrypt(dkpke, c);
    let mut g_in = Vec::with_capacity(64);
    g_in.extend_from_slice(&m2);
    g_in.extend_from_slice(h);
    let g = sha3_512(&g_in);
    let mut key = [0u8; 32];
    key.copy_from_slice(&g[..32]);
    let mut r = [0u8; 32];
    r.copy_from_slice(&g[32..]);

    let mut kbar_in = Vec::with_capacity(32 + c.len());
    kbar_in.extend_from_slice(z);
    kbar_in.extend_from_slice(c);
    let mut kbar = [0u8; 32];
    shake256(&kbar_in, &mut kbar);

    let c2 = kpke_encrypt(ek, &m2, &r);
    // Constant-time: select key on match, else the rejection value.
    let eq = ct_eq(c, &c2);
    ct_select(&key, &kbar, eq)
}

/// A convenience encapsulation that draws `m` from the caller-supplied RNG bytes.
pub fn encaps(ek: &[u8], rand32: &[u8; 32]) -> ([u8; 32], Vec<u8>) {
    encaps_internal(ek, rand32)
}

fn ct_eq(a: &[u8], b: &[u8]) -> u8 {
    if a.len() != b.len() {
        return 0;
    }
    let mut d = 0u8;
    for i in 0..a.len() {
        d |= a[i] ^ b[i];
    }
    // 0xFF if equal, 0x00 otherwise (branchless).
    let x = d as u16;
    ((x.wrapping_sub(1)) >> 8) as u8
}
fn ct_select(a: &[u8; 32], b: &[u8; 32], mask: u8) -> [u8; 32] {
    let mut o = [0u8; 32];
    for i in 0..32 {
        o[i] = (a[i] & mask) | (b[i] & !mask);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntt_roundtrip_is_identity() {
        let mut p = [0i16; N];
        for (i, c) in p.iter_mut().enumerate() {
            *c = ((i * 7 + 3) % Q as usize) as i16;
        }
        let orig = p;
        ntt(&mut p);
        invntt(&mut p);
        assert_eq!(p[..], orig[..]);
    }

    #[test]
    fn keygen_encaps_decaps_roundtrip() {
        let d = [7u8; 32];
        let z = [9u8; 32];
        let (ek, dk) = keygen(&d, &z);
        assert_eq!(ek.len(), EK_LEN);
        assert_eq!(dk.len(), DK_LEN);
        let m = [0x42u8; 32];
        let (ss1, c) = encaps_internal(&ek, &m);
        assert_eq!(c.len(), CT_LEN);
        let ss2 = decaps(&dk, &c);
        assert_eq!(ss1, ss2);
    }

    #[test]
    fn tampered_ciphertext_gives_rejection_not_shared_secret() {
        let (ek, dk) = keygen(&[1u8; 32], &[2u8; 32]);
        let (ss1, mut c) = encaps_internal(&ek, &[0x55u8; 32]);
        c[0] ^= 0xFF; // corrupt
        let ss2 = decaps(&dk, &c);
        // Implicit rejection: still 32 bytes, but NOT the real shared secret.
        assert_ne!(ss1, ss2);
    }
}
