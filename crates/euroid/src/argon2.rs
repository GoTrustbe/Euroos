//! Sovereign **Argon2id** password hashing (RFC 9106) — from-scratch, `no_std`.
//!
//! EuroOS hashes passwords exclusively with Argon2id — never MD5/SHA1/bcrypt, and
//! never "negotiated down". The memory-hard KDF makes GPU/ASIC brute force
//! unaffordable. We build it on our own **Blake2b** (RFC 7693), so there is no
//! external crypto dependency. Correctness is anchored to the official
//! RFC 9106 test vector (see `tests`).

use alloc::vec;
use alloc::vec::Vec;

// ─────────────────────────────────────────────────────────────────────────────
// Blake2b (RFC 7693) — unkeyed, variable output 1..=64 bytes.
// ─────────────────────────────────────────────────────────────────────────────

const BLAKE2B_IV: [u64; 8] = [
    0x6a09e667f3bcc908,
    0xbb67ae8584caa73b,
    0x3c6ef372fe94f82b,
    0xa54ff53a5f1d36f1,
    0x510e527fade682d1,
    0x9b05688c2b3e6c1f,
    0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];

const SIGMA: [[usize; 16]; 12] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
];

struct Blake2b {
    h: [u64; 8],
    t: [u64; 2],
    buf: [u8; 128],
    buflen: usize,
    outlen: usize,
}

impl Blake2b {
    fn new(outlen: usize) -> Self {
        let mut h = BLAKE2B_IV;
        // Parameter block for unkeyed hash: digest_length | (key_length<<8) | (fanout<<16) | (depth<<24)
        h[0] ^= 0x0101_0000 ^ (outlen as u64);
        Blake2b { h, t: [0, 0], buf: [0u8; 128], buflen: 0, outlen }
    }

    #[allow(clippy::too_many_arguments)]
    fn mix(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
        v[d] = (v[d] ^ v[a]).rotate_right(32);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(24);
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
        v[d] = (v[d] ^ v[a]).rotate_right(16);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(63);
    }

    fn compress(&mut self, last: bool) {
        let mut m = [0u64; 16];
        for i in 0..16 {
            let mut w = [0u8; 8];
            w.copy_from_slice(&self.buf[i * 8..i * 8 + 8]);
            m[i] = u64::from_le_bytes(w);
        }
        let mut v = [0u64; 16];
        v[..8].copy_from_slice(&self.h);
        v[8..].copy_from_slice(&BLAKE2B_IV);
        v[12] ^= self.t[0];
        v[13] ^= self.t[1];
        if last {
            v[14] = !v[14];
        }
        for r in 0..12 {
            let s = &SIGMA[r];
            Self::mix(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
            Self::mix(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
            Self::mix(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
            Self::mix(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
            Self::mix(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
            Self::mix(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
            Self::mix(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
            Self::mix(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
        }
        for i in 0..8 {
            self.h[i] ^= v[i] ^ v[i + 8];
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            if self.buflen == 128 {
                // A full block is only processed once MORE data follows, so that the
                // last block (with the last-flag) always goes through finalize.
                self.t[0] = self.t[0].wrapping_add(128);
                if self.t[0] < 128 {
                    self.t[1] = self.t[1].wrapping_add(1);
                }
                self.compress(false);
                self.buflen = 0;
            }
            let take = core::cmp::min(128 - self.buflen, data.len());
            self.buf[self.buflen..self.buflen + take].copy_from_slice(&data[..take]);
            self.buflen += take;
            data = &data[take..];
        }
    }

    fn finalize(mut self, out: &mut [u8]) {
        self.t[0] = self.t[0].wrapping_add(self.buflen as u64);
        if self.t[0] < self.buflen as u64 {
            self.t[1] = self.t[1].wrapping_add(1);
        }
        for i in self.buflen..128 {
            self.buf[i] = 0;
        }
        self.compress(true);
        let mut bytes = [0u8; 64];
        for i in 0..8 {
            bytes[i * 8..i * 8 + 8].copy_from_slice(&self.h[i].to_le_bytes());
        }
        out.copy_from_slice(&bytes[..self.outlen]);
    }
}

/// Blake2b with variable digest length `outlen` (1..=64).
pub fn blake2b(outlen: usize, data: &[u8]) -> Vec<u8> {
    let mut h = Blake2b::new(outlen);
    h.update(data);
    let mut out = vec![0u8; outlen];
    h.finalize(&mut out);
    out
}

/// The variable-length hash function H' from RFC 9106 §3.2 (extends Blake2b beyond
/// 64 bytes by chaining blocks together).
fn h_prime(outlen: usize, input: &[u8]) -> Vec<u8> {
    let mut prefixed = Vec::with_capacity(4 + input.len());
    prefixed.extend_from_slice(&(outlen as u32).to_le_bytes());
    prefixed.extend_from_slice(input);
    if outlen <= 64 {
        return blake2b(outlen, &prefixed);
    }
    let r = outlen.div_ceil(32) - 2;
    let mut out = Vec::with_capacity(outlen);
    let mut v = blake2b(64, &prefixed);
    out.extend_from_slice(&v[..32]);
    for _ in 1..r {
        v = blake2b(64, &v);
        out.extend_from_slice(&v[..32]);
    }
    let last_len = outlen - 32 * r;
    let last = blake2b(last_len, &v);
    out.extend_from_slice(&last);
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Argon2id (RFC 9106).
// ─────────────────────────────────────────────────────────────────────────────

const ARGON2_BLOCK_WORDS: usize = 128; // 1024 bytes / 8
const SYNC_POINTS: usize = 4;
const ADDRESSES_IN_BLOCK: usize = 128;
const ARGON2ID_TYPE: u64 = 2;
const ARGON2_VERSION: u32 = 0x13;

#[inline]
fn lo(x: u64) -> u64 {
    x & 0xffff_ffff
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn gb(v: &mut [u64; ARGON2_BLOCK_WORDS], a: usize, b: usize, c: usize, d: usize) {
    let t1 = lo(v[a]).wrapping_mul(lo(v[b]));
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(t1).wrapping_add(t1);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    let t2 = lo(v[c]).wrapping_mul(lo(v[d]));
    v[c] = v[c].wrapping_add(v[d]).wrapping_add(t2).wrapping_add(t2);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    let t3 = lo(v[a]).wrapping_mul(lo(v[b]));
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(t3).wrapping_add(t3);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    let t4 = lo(v[c]).wrapping_mul(lo(v[d]));
    v[c] = v[c].wrapping_add(v[d]).wrapping_add(t4).wrapping_add(t4);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

fn permutation_round(block: &mut [u64; ARGON2_BLOCK_WORDS], idx: [usize; 16]) {
    gb(block, idx[0], idx[4], idx[8], idx[12]);
    gb(block, idx[1], idx[5], idx[9], idx[13]);
    gb(block, idx[2], idx[6], idx[10], idx[14]);
    gb(block, idx[3], idx[7], idx[11], idx[15]);
    gb(block, idx[0], idx[5], idx[10], idx[15]);
    gb(block, idx[1], idx[6], idx[11], idx[12]);
    gb(block, idx[2], idx[7], idx[8], idx[13]);
    gb(block, idx[3], idx[4], idx[9], idx[14]);
}

/// The compression function G: `out = P(R) XOR R` with `R = prev XOR refb` (RFC 9106 §3.5).
/// With `with_xor` the existing content of `out` is also XOR'd in (passes >0).
fn fill_block(
    prev: &[u64; ARGON2_BLOCK_WORDS],
    refb: &[u64; ARGON2_BLOCK_WORDS],
    out: &mut [u64; ARGON2_BLOCK_WORDS],
    with_xor: bool,
) {
    let mut r = [0u64; ARGON2_BLOCK_WORDS];
    for i in 0..ARGON2_BLOCK_WORDS {
        r[i] = prev[i] ^ refb[i];
    }
    let mut block = r;
    // 8 row rounds: 16 consecutive words per row.
    for i in 0..8 {
        let base = 16 * i;
        let idx = [
            base,
            base + 1,
            base + 2,
            base + 3,
            base + 4,
            base + 5,
            base + 6,
            base + 7,
            base + 8,
            base + 9,
            base + 10,
            base + 11,
            base + 12,
            base + 13,
            base + 14,
            base + 15,
        ];
        permutation_round(&mut block, idx);
    }
    // 8 column rounds: registers of 2 words, row stride 16 words.
    for i in 0..8 {
        let b = 2 * i;
        let idx = [
            b,
            b + 1,
            b + 16,
            b + 17,
            b + 32,
            b + 33,
            b + 48,
            b + 49,
            b + 64,
            b + 65,
            b + 80,
            b + 81,
            b + 96,
            b + 97,
            b + 112,
            b + 113,
        ];
        permutation_round(&mut block, idx);
    }
    if with_xor {
        for i in 0..ARGON2_BLOCK_WORDS {
            out[i] = block[i] ^ r[i] ^ out[i];
        }
    } else {
        for i in 0..ARGON2_BLOCK_WORDS {
            out[i] = block[i] ^ r[i];
        }
    }
}

fn bytes_to_block(b: &[u8]) -> [u64; ARGON2_BLOCK_WORDS] {
    let mut out = [0u64; ARGON2_BLOCK_WORDS];
    for i in 0..ARGON2_BLOCK_WORDS {
        let mut w = [0u8; 8];
        w.copy_from_slice(&b[i * 8..i * 8 + 8]);
        out[i] = u64::from_le_bytes(w);
    }
    out
}

fn block_to_bytes(blk: &[u64; ARGON2_BLOCK_WORDS]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1024);
    for w in blk.iter() {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

/// Argon2 parameters (memory cost in KiB, iterations, parallel lanes, tag length).
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub tag_len: usize,
}

/// Compute the Argon2id tag for (`pwd`, `salt`, optional `secret`/`ad`).
pub fn argon2id(pwd: &[u8], salt: &[u8], secret: &[u8], ad: &[u8], p: &Params) -> Vec<u8> {
    let lanes = p.p_cost.max(1) as usize;
    let tag_len = p.tag_len;

    // m' = 4*p*floor(m/(4p)), with a minimum of 8*p blocks.
    let mut m_prime = p.m_cost as usize;
    let min_mem = 8 * lanes;
    if m_prime < min_mem {
        m_prime = min_mem;
    }
    m_prime = (m_prime / (4 * lanes)) * (4 * lanes);
    let lane_len = m_prime / lanes; // q (columns per lane)
    let seg_len = lane_len / SYNC_POINTS;

    // H0 (64 bytes).
    let mut h0_in = Vec::new();
    h0_in.extend_from_slice(&(lanes as u32).to_le_bytes());
    h0_in.extend_from_slice(&(tag_len as u32).to_le_bytes());
    h0_in.extend_from_slice(&p.m_cost.to_le_bytes());
    h0_in.extend_from_slice(&p.t_cost.to_le_bytes());
    h0_in.extend_from_slice(&ARGON2_VERSION.to_le_bytes());
    h0_in.extend_from_slice(&(ARGON2ID_TYPE as u32).to_le_bytes());
    h0_in.extend_from_slice(&(pwd.len() as u32).to_le_bytes());
    h0_in.extend_from_slice(pwd);
    h0_in.extend_from_slice(&(salt.len() as u32).to_le_bytes());
    h0_in.extend_from_slice(salt);
    h0_in.extend_from_slice(&(secret.len() as u32).to_le_bytes());
    h0_in.extend_from_slice(secret);
    h0_in.extend_from_slice(&(ad.len() as u32).to_le_bytes());
    h0_in.extend_from_slice(ad);
    let h0 = blake2b(64, &h0_in);

    // Memory: m' blocks of 1024 bytes.
    let mut mem: Vec<[u64; ARGON2_BLOCK_WORDS]> = vec![[0u64; ARGON2_BLOCK_WORDS]; m_prime];

    // The first two blocks of each lane.
    for lane in 0..lanes {
        let mut in0 = Vec::with_capacity(72);
        in0.extend_from_slice(&h0);
        in0.extend_from_slice(&0u32.to_le_bytes());
        in0.extend_from_slice(&(lane as u32).to_le_bytes());
        mem[lane * lane_len] = bytes_to_block(&h_prime(1024, &in0));

        let mut in1 = Vec::with_capacity(72);
        in1.extend_from_slice(&h0);
        in1.extend_from_slice(&1u32.to_le_bytes());
        in1.extend_from_slice(&(lane as u32).to_le_bytes());
        mem[lane * lane_len + 1] = bytes_to_block(&h_prime(1024, &in1));
    }

    let passes = p.t_cost.max(1) as usize;
    for pass in 0..passes {
        for slice in 0..SYNC_POINTS {
            // Argon2id: data-independent addressing in pass 0, slices 0 and 1.
            let data_independent = pass == 0 && slice < 2;
            for lane in 0..lanes {
                fill_segment(
                    &mut mem,
                    pass,
                    lane,
                    slice,
                    data_independent,
                    lanes,
                    lane_len,
                    seg_len,
                    m_prime,
                    passes,
                );
            }
        }
    }

    // Final block = XOR of the last block of each lane.
    let mut final_block = mem[lane_len - 1];
    for lane in 1..lanes {
        let b = mem[lane * lane_len + lane_len - 1];
        for i in 0..ARGON2_BLOCK_WORDS {
            final_block[i] ^= b[i];
        }
    }
    h_prime(tag_len, &block_to_bytes(&final_block))
}

#[allow(clippy::too_many_arguments)]
fn fill_segment(
    mem: &mut [[u64; ARGON2_BLOCK_WORDS]],
    pass: usize,
    lane: usize,
    slice: usize,
    data_independent: bool,
    lanes: usize,
    lane_len: usize,
    seg_len: usize,
    m_prime: usize,
    passes: usize,
) {
    let zero_block = [0u64; ARGON2_BLOCK_WORDS];
    let mut input_block = [0u64; ARGON2_BLOCK_WORDS];
    let mut address_block = [0u64; ARGON2_BLOCK_WORDS];
    if data_independent {
        input_block[0] = pass as u64;
        input_block[1] = lane as u64;
        input_block[2] = slice as u64;
        input_block[3] = m_prime as u64;
        input_block[4] = passes as u64;
        input_block[5] = ARGON2ID_TYPE;
    }

    for i in 0..seg_len {
        // Generate pseudo-randomness. With data-independent addressing we refresh
        // each address block per 128 indices — also for the skipped first blocks.
        let mut rand_di: u64 = 0;
        if data_independent {
            if i % ADDRESSES_IN_BLOCK == 0 {
                input_block[6] = input_block[6].wrapping_add(1);
                fill_block(&zero_block, &input_block, &mut address_block, false);
                let copy = address_block;
                fill_block(&zero_block, &copy, &mut address_block, false);
            }
            rand_di = address_block[i % ADDRESSES_IN_BLOCK];
        }

        // The first two blocks of pass 0 / slice 0 are already filled.
        if pass == 0 && slice == 0 && i < 2 {
            continue;
        }

        let curr = lane * lane_len + slice * seg_len + i;
        let prev = if curr % lane_len == 0 { curr + lane_len - 1 } else { curr - 1 };

        let rand = if data_independent { rand_di } else { mem[prev][0] };
        let j1 = rand & 0xffff_ffff;
        let j2 = rand >> 32;

        let ref_lane = if pass == 0 && slice == 0 { lane } else { (j2 as usize) % lanes };

        let ref_area_size: usize = if pass == 0 {
            if slice == 0 {
                i - 1
            } else if ref_lane == lane {
                slice * seg_len + i - 1
            } else {
                slice * seg_len - usize::from(i == 0)
            }
        } else if ref_lane == lane {
            lane_len - seg_len + i - 1
        } else {
            lane_len - seg_len - usize::from(i == 0)
        };

        let x = (j1.wrapping_mul(j1)) >> 32;
        let y = (ref_area_size as u64).wrapping_mul(x) >> 32;
        let z = ref_area_size as u64 - 1 - y;

        let start_pos = if pass != 0 && slice != SYNC_POINTS - 1 {
            (slice + 1) * seg_len
        } else {
            0
        };
        let ref_index = (start_pos + z as usize) % lane_len;
        let ref_off = ref_lane * lane_len + ref_index;

        let prev_block = mem[prev];
        let ref_block = mem[ref_off];
        let mut out = mem[curr];
        fill_block(&prev_block, &ref_block, &mut out, pass != 0);
        mem[curr] = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 7693 Appendix A: Blake2b-512("abc").
    #[test]
    fn blake2b_abc() {
        let d = blake2b(64, b"abc");
        let expect: [u8; 64] = [
            0xba, 0x80, 0xa5, 0x3f, 0x98, 0x1c, 0x4d, 0x0d, 0x6a, 0x27, 0x97, 0xb6, 0x9f, 0x12,
            0xf6, 0xe9, 0x4c, 0x21, 0x2f, 0x14, 0x68, 0x5a, 0xc4, 0xb7, 0x4b, 0x12, 0xbb, 0x6f,
            0xdb, 0xff, 0xa2, 0xd1, 0x7d, 0x87, 0xc5, 0x39, 0x2a, 0xab, 0x79, 0x2d, 0xc2, 0x52,
            0xd5, 0xde, 0x45, 0x33, 0xcc, 0x95, 0x18, 0xd3, 0x8a, 0xa8, 0xdb, 0xf1, 0x92, 0x5a,
            0xb9, 0x23, 0x86, 0xed, 0xd4, 0x00, 0x99, 0x23,
        ];
        assert_eq!(d, expect);
    }

    // RFC 9106 §5.3: official Argon2id test vector.
    #[test]
    fn argon2id_rfc9106_vector() {
        let pwd = [0x01u8; 32];
        let salt = [0x02u8; 16];
        let secret = [0x03u8; 8];
        let ad = [0x04u8; 12];
        let params = Params { m_cost: 32, t_cost: 3, p_cost: 4, tag_len: 32 };
        let tag = argon2id(&pwd, &salt, &secret, &ad, &params);
        let expect: [u8; 32] = [
            0x0d, 0x64, 0x0d, 0xf5, 0x8d, 0x78, 0x76, 0x6c, 0x08, 0xc0, 0x37, 0xa3, 0x4a, 0x8b,
            0x53, 0xc9, 0xd0, 0x1e, 0xf0, 0x45, 0x2d, 0x75, 0xb6, 0x5e, 0xb5, 0x25, 0x20, 0xe9,
            0x6b, 0x01, 0xe6, 0x59,
        ];
        assert_eq!(tag, expect, "Argon2id RFC 9106 test vector must match exactly");
    }
}
