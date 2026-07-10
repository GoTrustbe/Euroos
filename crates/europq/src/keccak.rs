//! Keccak-f[1600] + the SHA-3 / SHAKE functions ML-KEM needs, from scratch.
//!
//! FIPS 202. A single sponge over the 24-round Keccak permutation gives SHA3-256
//! (H), SHA3-512 (G), SHAKE-128 (XOF for matrix expansion) and SHAKE-256 (PRF /
//! J). No external crate — the whole hash layer under EuroPQ is our own code.

const RC: [u64; 24] = [
    0x0000_0000_0000_0001,
    0x0000_0000_0000_8082,
    0x8000_0000_0000_808a,
    0x8000_0000_8000_8000,
    0x0000_0000_0000_808b,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8009,
    0x0000_0000_0000_008a,
    0x0000_0000_0000_0088,
    0x0000_0000_8000_8009,
    0x0000_0000_8000_000a,
    0x0000_0000_8000_808b,
    0x8000_0000_0000_008b,
    0x8000_0000_0000_8089,
    0x8000_0000_0000_8003,
    0x8000_0000_0000_8002,
    0x8000_0000_0000_0080,
    0x0000_0000_0000_800a,
    0x8000_0000_8000_000a,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8080,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8008,
];

const ROT: [[u32; 5]; 5] = [
    [0, 36, 3, 41, 18],
    [1, 44, 10, 45, 2],
    [62, 6, 43, 15, 61],
    [28, 55, 25, 21, 56],
    [27, 20, 39, 8, 14],
];

fn keccak_f(a: &mut [u64; 25]) {
    for &rc in RC.iter() {
        // θ
        let mut c = [0u64; 5];
        for x in 0..5 {
            c[x] = a[x] ^ a[x + 5] ^ a[x + 10] ^ a[x + 15] ^ a[x + 20];
        }
        let mut d = [0u64; 5];
        for x in 0..5 {
            d[x] = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
        }
        for x in 0..5 {
            for y in 0..5 {
                a[x + 5 * y] ^= d[x];
            }
        }
        // ρ and π
        let mut b = [0u64; 25];
        for x in 0..5 {
            for y in 0..5 {
                b[y + 5 * ((2 * x + 3 * y) % 5)] = a[x + 5 * y].rotate_left(ROT[x][y]);
            }
        }
        // χ
        for x in 0..5 {
            for y in 0..5 {
                a[x + 5 * y] = b[x + 5 * y] ^ ((!b[(x + 1) % 5 + 5 * y]) & b[(x + 2) % 5 + 5 * y]);
            }
        }
        // ι
        a[0] ^= rc;
    }
}

/// A streaming Keccak sponge with a given rate (bytes) and domain suffix.
pub struct Sponge {
    state: [u64; 25],
    rate: usize,
    suffix: u8,
    offset: usize, // absorb byte position within the current rate block
    squeezing: bool,
    sq_off: usize, // squeeze byte position
}

impl Sponge {
    pub fn new(rate: usize, suffix: u8) -> Self {
        Sponge { state: [0u64; 25], rate, suffix, offset: 0, squeezing: false, sq_off: 0 }
    }

    fn xor_byte(&mut self, pos: usize, b: u8) {
        let lane = pos / 8;
        let shift = (pos % 8) * 8;
        self.state[lane] ^= (b as u64) << shift;
    }

    fn get_byte(&self, pos: usize) -> u8 {
        let lane = pos / 8;
        let shift = (pos % 8) * 8;
        (self.state[lane] >> shift) as u8
    }

    pub fn absorb(&mut self, data: &[u8]) {
        for &b in data {
            self.xor_byte(self.offset, b);
            self.offset += 1;
            if self.offset == self.rate {
                keccak_f(&mut self.state);
                self.offset = 0;
            }
        }
    }

    fn pad(&mut self) {
        // domain suffix, then the final 0x80 at the end of the rate block.
        self.xor_byte(self.offset, self.suffix);
        self.xor_byte(self.rate - 1, 0x80);
        keccak_f(&mut self.state);
        self.squeezing = true;
        self.sq_off = 0;
    }

    pub fn squeeze(&mut self, out: &mut [u8]) {
        if !self.squeezing {
            self.pad();
        }
        for o in out.iter_mut() {
            if self.sq_off == self.rate {
                keccak_f(&mut self.state);
                self.sq_off = 0;
            }
            *o = self.get_byte(self.sq_off);
            self.sq_off += 1;
        }
    }
}

fn hash(rate: usize, suffix: u8, input: &[u8], out: &mut [u8]) {
    let mut s = Sponge::new(rate, suffix);
    s.absorb(input);
    s.squeeze(out);
}

/// SHA3-256 (H in ML-KEM).
pub fn sha3_256(input: &[u8]) -> [u8; 32] {
    let mut o = [0u8; 32];
    hash(136, 0x06, input, &mut o);
    o
}

/// SHA3-512 (G in ML-KEM).
pub fn sha3_512(input: &[u8]) -> [u8; 64] {
    let mut o = [0u8; 64];
    hash(72, 0x06, input, &mut o);
    o
}

/// SHAKE-256 with a fixed output length (PRF / J in ML-KEM).
pub fn shake256(input: &[u8], out: &mut [u8]) {
    hash(136, 0x1f, input, out);
}

/// A SHAKE-128 XOF handle for streaming squeezes (matrix expansion in ML-KEM).
pub fn shake128_xof(input: &[u8]) -> Sponge {
    let mut s = Sponge::new(168, 0x1f);
    s.absorb(input);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hx(s: &str) -> alloc::vec::Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    // FIPS 202 known-answer vectors for the empty message.
    #[test]
    fn sha3_256_empty() {
        assert_eq!(
            sha3_256(b"")[..],
            hx("a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a")[..]
        );
    }

    #[test]
    fn sha3_512_empty() {
        assert_eq!(
            sha3_512(b"")[..],
            hx("a69f73cca23a9ac5c8b567dc185a756e97c982164fe25859e0d1dcc1475c80a615b2123af1f5f94c11e3e9402c3ac558f500199d95b6d3e301758586281dcd26")[..]
        );
    }

    #[test]
    fn sha3_256_abc() {
        assert_eq!(
            sha3_256(b"abc")[..],
            hx("3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532")[..]
        );
    }

    #[test]
    fn shake256_empty_prefix() {
        // First 32 bytes of SHAKE256("").
        let mut o = [0u8; 32];
        shake256(b"", &mut o);
        assert_eq!(o[..], hx("46b9dd2b0ba88d13233b3feb743eeb243fcd52ea62b81b82b50c27646ed5762f")[..]);
    }
}
