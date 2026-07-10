//! RFC 1951 **INFLATE** — decodes stored, fixed-Huffman and **dynamic-Huffman**
//! DEFLATE blocks. Dynamic Huffman is the case real `.docx`/`.xlsx`/`.zip`
//! writers use, so it must be correct; the crate's KAT test decodes real
//! `zlib`-level-9 streams byte-for-byte.

use alloc::vec;
use alloc::vec::Vec;

#[derive(Debug, PartialEq, Eq)]
pub enum InflateError {
    /// Ran out of input mid-stream.
    UnexpectedEof,
    /// A reserved/invalid block type (BTYPE=11).
    BadBlockType,
    /// A stored block's LEN/NLEN complement check failed.
    BadStoredLen,
    /// A Huffman code with no symbol / an invalid distance or length code.
    BadCode,
    /// A back-reference points before the start of the output.
    BadDistance,
    /// The output grew past the caller's sanity limit.
    TooLarge,
}

/// The largest output we will produce (guards a malicious stream). 64 MiB.
const MAX_OUTPUT: usize = 64 * 1024 * 1024;

/// LSB-first bit reader over a byte slice (DEFLATE is little-endian bit order).
struct BitReader<'a> {
    data: &'a [u8],
    /// Byte position.
    pos: usize,
    /// Bit buffer + how many valid bits it holds.
    bits: u32,
    nbits: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, bits: 0, nbits: 0 }
    }

    #[inline]
    fn need(&mut self, n: u32) -> Result<(), InflateError> {
        while self.nbits < n {
            let byte = *self.data.get(self.pos).ok_or(InflateError::UnexpectedEof)?;
            self.pos += 1;
            self.bits |= (byte as u32) << self.nbits;
            self.nbits += 8;
        }
        Ok(())
    }

    /// Read `n` bits (0..=32-ish), LSB first.
    #[inline]
    fn bits(&mut self, n: u32) -> Result<u32, InflateError> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.bits & ((1u32 << n) - 1);
        self.bits >>= n;
        self.nbits -= n;
        Ok(v)
    }

    /// Discard bits up to the next byte boundary (for stored blocks).
    fn align(&mut self) {
        let drop = self.nbits % 8;
        self.bits >>= drop;
        self.nbits -= drop;
    }
}

/// A canonical Huffman decoder built from a list of code lengths.
struct Huffman {
    /// Fast path: for codes ≤ FAST_BITS, table[peek] = (symbol<<4 | len).
    fast: Vec<u16>,
    /// Fallback for longer codes: sorted (code, len, symbol) via the canonical
    /// first-code arrays.
    counts: [u16; MAX_BITS + 1],
    symbols: Vec<u16>,
    max_len: u32,
}

const MAX_BITS: usize = 15;
const FAST_BITS: u32 = 9;

impl Huffman {
    /// Build from per-symbol code lengths (0 = symbol unused).
    fn new(lengths: &[u8]) -> Result<Self, InflateError> {
        let mut counts = [0u16; MAX_BITS + 1];
        let mut max_len = 0u32;
        for &l in lengths {
            counts[l as usize] += 1;
            if l as u32 > max_len {
                max_len = l as u32;
            }
        }
        counts[0] = 0;

        // Canonical order: symbols sorted by (length, symbol).
        let mut offsets = [0u16; MAX_BITS + 2];
        for i in 1..=MAX_BITS {
            offsets[i + 1] = offsets[i] + counts[i];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offsets[l as usize] as usize] = sym as u16;
                offsets[l as usize] += 1;
            }
        }

        // Fast lookup table for codes up to FAST_BITS.
        let fast_size = 1usize << FAST_BITS;
        let mut fast = vec![0u16; fast_size];
        // Assign canonical codes and fill the (bit-reversed) fast table.
        let mut code = 0u32;
        let mut sym_index = 0usize;
        for len in 1..=MAX_BITS as u32 {
            for _ in 0..counts[len as usize] {
                let sym = symbols[sym_index];
                sym_index += 1;
                if len <= FAST_BITS {
                    // DEFLATE reads LSB-first, so the fast index is the reversed code.
                    let rev = reverse_bits(code, len);
                    let entry = (sym << 4) | len as u16;
                    let step = 1usize << len;
                    let mut i = rev as usize;
                    while i < fast_size {
                        fast[i] = entry;
                        i += step;
                    }
                }
                code += 1;
            }
            code <<= 1;
        }

        Ok(Self { fast, counts, symbols, max_len })
    }

    /// Decode one symbol from the reader.
    fn decode(&self, br: &mut BitReader) -> Result<u16, InflateError> {
        // Fast path.
        br.need(FAST_BITS).or_else(|e| {
            // Near EOF there may be < FAST_BITS left; that's fine as long as the
            // real code fits — fall through to the slow path which reads bit by bit.
            if matches!(e, InflateError::UnexpectedEof) {
                Ok(())
            } else {
                Err(e)
            }
        })?;
        if br.nbits >= FAST_BITS {
            let peek = (br.bits & ((1 << FAST_BITS) - 1)) as usize;
            let entry = self.fast[peek];
            let len = (entry & 0x0F) as u32;
            if len != 0 {
                br.bits >>= len;
                br.nbits -= len;
                return Ok(entry >> 4);
            }
        }
        // Slow path: accumulate the code bit by bit past FAST_BITS.
        let mut code = 0u32;
        let mut first = 0u32;
        let mut index = 0u32;
        for len in 1..=self.max_len {
            code |= br.bits(1)?;
            let count = self.counts[len as usize] as u32;
            if code < first + count {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(InflateError::BadCode)
    }
}

fn reverse_bits(mut v: u32, n: u32) -> u32 {
    let mut r = 0u32;
    for _ in 0..n {
        r = (r << 1) | (v & 1);
        v >>= 1;
    }
    r
}

// Length/distance base + extra-bit tables (RFC 1951 §3.2.5).
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

/// The order in which the 19 code-length-code lengths appear (RFC 1951 §3.2.7).
const CLCL_ORDER: [usize; 19] =
    [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// Fixed Huffman literal/length code lengths (RFC 1951 §3.2.6).
fn fixed_litlen() -> Huffman {
    let mut lens = [0u8; 288];
    for (i, l) in lens.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Huffman::new(&lens).expect("fixed litlen")
}

fn fixed_dist() -> Huffman {
    let lens = [5u8; 30];
    Huffman::new(&lens).expect("fixed dist")
}

/// Inflate a raw DEFLATE stream (no zlib/gzip wrapper).
pub fn inflate(data: &[u8]) -> Result<Vec<u8>, InflateError> {
    let mut br = BitReader::new(data);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let bfinal = br.bits(1)?;
        let btype = br.bits(2)?;
        match btype {
            0 => inflate_stored(&mut br, &mut out)?,
            1 => inflate_block(&mut br, &mut out, &fixed_litlen(), &fixed_dist())?,
            2 => {
                let (litlen, dist) = read_dynamic_tables(&mut br)?;
                inflate_block(&mut br, &mut out, &litlen, &dist)?;
            }
            _ => return Err(InflateError::BadBlockType),
        }
        if out.len() > MAX_OUTPUT {
            return Err(InflateError::TooLarge);
        }
        if bfinal == 1 {
            break;
        }
    }
    Ok(out)
}

fn inflate_stored(br: &mut BitReader, out: &mut Vec<u8>) -> Result<(), InflateError> {
    br.align();
    let len = br.bits(16)? as usize;
    let nlen = br.bits(16)? as usize;
    if len ^ 0xFFFF != nlen {
        return Err(InflateError::BadStoredLen);
    }
    for _ in 0..len {
        out.push(br.bits(8)? as u8);
    }
    Ok(())
}

fn read_dynamic_tables(br: &mut BitReader) -> Result<(Huffman, Huffman), InflateError> {
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;

    // Code-length-code lengths, in the special order.
    let mut cl_lens = [0u8; 19];
    for i in 0..hclen {
        cl_lens[CLCL_ORDER[i]] = br.bits(3)? as u8;
    }
    let cl_huff = Huffman::new(&cl_lens)?;

    // Decode the literal/length + distance code lengths (run-length encoded).
    let total = hlit + hdist;
    let mut lens: Vec<u8> = Vec::with_capacity(total);
    while lens.len() < total {
        let sym = cl_huff.decode(br)?;
        match sym {
            0..=15 => lens.push(sym as u8),
            16 => {
                // Repeat the previous length 3..6 times.
                let prev = *lens.last().ok_or(InflateError::BadCode)?;
                let rep = 3 + br.bits(2)? as usize;
                lens.resize(lens.len() + rep, prev);
            }
            17 => {
                let rep = 3 + br.bits(3)? as usize;
                lens.resize(lens.len() + rep, 0);
            }
            18 => {
                let rep = 11 + br.bits(7)? as usize;
                lens.resize(lens.len() + rep, 0);
            }
            _ => return Err(InflateError::BadCode),
        }
    }
    if lens.len() != total {
        return Err(InflateError::BadCode);
    }
    let litlen = Huffman::new(&lens[..hlit])?;
    let dist = Huffman::new(&lens[hlit..])?;
    Ok((litlen, dist))
}

fn inflate_block(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    litlen: &Huffman,
    dist: &Huffman,
) -> Result<(), InflateError> {
    loop {
        let sym = litlen.decode(br)?;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Ok(()), // end of block
            257..=285 => {
                let li = (sym - 257) as usize;
                if li >= LEN_BASE.len() {
                    return Err(InflateError::BadCode);
                }
                let length = LEN_BASE[li] as usize + br.bits(LEN_EXTRA[li] as u32)? as usize;
                let dsym = dist.decode(br)? as usize;
                if dsym >= DIST_BASE.len() {
                    return Err(InflateError::BadCode);
                }
                let distance = DIST_BASE[dsym] as usize + br.bits(DIST_EXTRA[dsym] as u32)? as usize;
                if distance == 0 || distance > out.len() {
                    return Err(InflateError::BadDistance);
                }
                // Copy `length` bytes from `distance` back — may overlap (RLE).
                let start = out.len() - distance;
                for i in 0..length {
                    let b = out[start + i];
                    out.push(b);
                }
            }
            _ => return Err(InflateError::BadCode),
        }
    }
}
