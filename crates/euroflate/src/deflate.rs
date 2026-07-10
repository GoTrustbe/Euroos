//! RFC 1951 **DEFLATE** compressor — LZ77 (hash-chain match finder) emitted with
//! the **fixed Huffman** code (BTYPE=01). Fixed Huffman is always a valid DEFLATE
//! stream, so real tools (`zlib`, LibreOffice, `unzip`) read our output; the
//! crate test proves it by round-tripping through real `zlib` on the host.
//! We don't emit dynamic Huffman on the write side (more compact but far more
//! complex) — reading dynamic Huffman is what matters for interop and INFLATE
//! handles that fully.

use alloc::vec::Vec;

use crate::inflate::InflateError;

/// LSB-first bit writer.
struct BitWriter {
    out: Vec<u8>,
    bits: u32,
    nbits: u32,
}

impl BitWriter {
    fn new() -> Self {
        Self { out: Vec::new(), bits: 0, nbits: 0 }
    }
    #[inline]
    fn write(&mut self, value: u32, n: u32) {
        // n ≤ 16 throughout (max is a 13-bit distance extra), so a u32 bit
        // buffer never overflows before it is drained to bytes below.
        debug_assert!(n <= 16);
        let mask = if n == 0 { 0 } else { (1u32 << n) - 1 };
        self.bits |= (value & mask) << self.nbits;
        self.nbits += n;
        while self.nbits >= 8 {
            self.out.push((self.bits & 0xFF) as u8);
            self.bits >>= 8;
            self.nbits -= 8;
        }
    }
    /// Write a Huffman code, which is stored MSB-first per RFC 1951 §3.1.1 —
    /// so we bit-reverse it into the LSB-first stream.
    fn write_code(&mut self, code: u32, n: u32) {
        let mut rev = 0u32;
        for i in 0..n {
            rev |= ((code >> i) & 1) << (n - 1 - i);
        }
        self.write(rev, n);
    }
    fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.out.push((self.bits & 0xFF) as u8);
        }
        self.out
    }
}

// Fixed-Huffman literal/length codes (RFC 1951 §3.2.6):
//   0..=143   → 8 bits, codes 0x30..0xBF
//   144..=255 → 9 bits, codes 0x190..0x1FF
//   256..=279 → 7 bits, codes 0x00..0x17
//   280..=287 → 8 bits, codes 0xC0..0xC7
fn litlen_code(sym: u16) -> (u32, u32) {
    match sym {
        0..=143 => (0x30 + sym as u32, 8),
        144..=255 => (0x190 + (sym as u32 - 144), 9),
        256..=279 => (sym as u32 - 256, 7),
        _ => (0xC0 + (sym as u32 - 280), 8),
    }
}

// Length/distance tables (same as inflate, needed to encode matches).
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn length_code(len: usize) -> (u16, u32, u32) {
    // Returns (litlen_symbol, extra_bits_value, extra_bits_count).
    let mut i = LEN_BASE.len() - 1;
    while i > 0 && (len as u16) < LEN_BASE[i] {
        i -= 1;
    }
    let sym = 257 + i as u16;
    let extra = len as u32 - LEN_BASE[i] as u32;
    (sym, extra, LEN_EXTRA[i] as u32)
}

fn distance_code(dist: usize) -> (u16, u32, u32) {
    let mut i = DIST_BASE.len() - 1;
    while i > 0 && (dist as u16) < DIST_BASE[i] {
        i -= 1;
    }
    (i as u16, dist as u32 - DIST_BASE[i] as u32, DIST_EXTRA[i] as u32)
}

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const WINDOW: usize = 32768;
const HASH_BITS: usize = 15;
const HASH_SIZE: usize = 1 << HASH_BITS;

#[inline]
fn hash3(d: &[u8], i: usize) -> usize {
    let h = (d[i] as usize) << 10 ^ (d[i + 1] as usize) << 5 ^ (d[i + 2] as usize);
    (h.wrapping_mul(2654435761)) & (HASH_SIZE - 1)
}

/// Compress `data` to a raw DEFLATE stream (fixed Huffman, one final block).
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut bw = BitWriter::new();
    bw.write(1, 1); // BFINAL = 1
    bw.write(1, 2); // BTYPE = 01 (fixed Huffman)

    // LZ77 with a hash-chain match finder.
    let n = data.len();
    let mut head = alloc::vec![usize::MAX; HASH_SIZE];
    let mut prev = alloc::vec![usize::MAX; n.max(1)];

    let emit_literal = |bw: &mut BitWriter, b: u8| {
        let (c, l) = litlen_code(b as u16);
        bw.write_code(c, l);
    };

    let mut i = 0;
    while i < n {
        let mut best_len = 0usize;
        let mut best_dist = 0usize;
        if i + MIN_MATCH <= n {
            let h = hash3(data, i);
            let mut cand = head[h];
            let mut chain = 0;
            while cand != usize::MAX && chain < 128 {
                let dist = i - cand;
                if dist > WINDOW {
                    break;
                }
                // Extend the match.
                let mut l = 0;
                while l < MAX_MATCH && i + l < n && data[cand + l] == data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = dist;
                    if l >= MAX_MATCH {
                        break;
                    }
                }
                cand = prev[cand];
                chain += 1;
            }
            // Insert the current position into the hash chain.
            prev[i] = head[h];
            head[h] = i;
        }

        if best_len >= MIN_MATCH {
            // Emit a length/distance pair.
            let (lsym, lextra, lbits) = length_code(best_len);
            let (lc, ll) = litlen_code(lsym);
            bw.write_code(lc, ll);
            bw.write(lextra, lbits);
            let (dsym, dextra, dbits) = distance_code(best_dist);
            // Distance codes are 5-bit fixed, MSB-first → reverse like litlen.
            bw.write_code(dsym as u32, 5);
            bw.write(dextra, dbits);
            // Insert the covered positions into the hash chains (skip i itself,
            // already inserted).
            let end = i + best_len;
            let mut j = i + 1;
            while j < end && j + MIN_MATCH <= n {
                let h = hash3(data, j);
                prev[j] = head[h];
                head[h] = j;
                j += 1;
            }
            i = end;
        } else {
            emit_literal(&mut bw, data[i]);
            i += 1;
        }
    }
    // End-of-block symbol 256.
    let (c, l) = litlen_code(256);
    bw.write_code(c, l);
    bw.finish()
}

/// Round-trip helper for callers that want a quick self-check.
pub fn deflate_then_inflate(data: &[u8]) -> Result<Vec<u8>, InflateError> {
    crate::inflate::inflate(&deflate(data))
}
