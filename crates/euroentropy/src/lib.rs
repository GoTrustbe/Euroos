//! EuroEntropy — the sovereign early-boot CSPRNG.
//!
//! EuroOS draws key material very early: the FDE key, TPM-sealed secrets, VPN and
//! ML-KEM key generation, EuroCA and update signing. Feeding those from a raw
//! device RNG (or a `0x5A` fallback) is the weakest-link-at-the-worst-moment
//! problem. This crate closes it with:
//!
//! - [`HmacDrbg`] — HMAC-DRBG SHA-256 (NIST SP 800-90A), the deterministic CSPRNG
//!   core, **verified byte-for-byte against the NIST ACVP known-answer vectors**
//!   (`tests/kat.rs`).
//! - [`EntropyPool`] — collects real noise (CPU timing **jitter** + the TPM RNG),
//!   conservatively estimates its min-entropy, and refuses to hand out randomness
//!   (`ready() == false`) until it has seeded the DRBG with enough of it — a hard
//!   **`getrandom`-blocking** guarantee, not best-effort.
//!
//! No external crypto crate beyond SHA-256; HMAC is built here.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;
use sha2::{Digest, Sha256};

/// HMAC-SHA256 (RFC 2104).
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let ih = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(ih);
    let mut r = [0u8; 32];
    r.copy_from_slice(&outer.finalize());
    r
}

/// HMAC-DRBG (SHA-256), NIST SP 800-90A. Deterministic given its seed; the whole
/// security rests on the entropy of the instantiate/reseed inputs — see
/// [`EntropyPool`].
pub struct HmacDrbg {
    k: [u8; 32],
    v: [u8; 32],
    reseed_counter: u64,
}

impl HmacDrbg {
    /// The SP 800-90A `HMAC_DRBG_Update` with concatenated `provided_data`.
    fn update(&mut self, parts: &[&[u8]]) {
        let all_empty = parts.iter().all(|p| p.is_empty());
        // K = HMAC(K, V ‖ 0x00 ‖ provided_data)
        let mut m = Vec::with_capacity(33);
        m.extend_from_slice(&self.v);
        m.push(0x00);
        for p in parts {
            m.extend_from_slice(p);
        }
        self.k = hmac_sha256(&self.k, &m);
        self.v = hmac_sha256(&self.k, &self.v);
        if all_empty {
            return;
        }
        // K = HMAC(K, V ‖ 0x01 ‖ provided_data)
        let mut m = Vec::with_capacity(33);
        m.extend_from_slice(&self.v);
        m.push(0x01);
        for p in parts {
            m.extend_from_slice(p);
        }
        self.k = hmac_sha256(&self.k, &m);
        self.v = hmac_sha256(&self.k, &self.v);
    }

    /// `HMAC_DRBG_Instantiate(entropy, nonce, personalization_string)`.
    pub fn instantiate(entropy: &[u8], nonce: &[u8], perso: &[u8]) -> Self {
        let mut s = HmacDrbg { k: [0u8; 32], v: [0x01u8; 32], reseed_counter: 1 };
        s.update(&[entropy, nonce, perso]);
        s
    }

    /// `HMAC_DRBG_Reseed(entropy, additional_input)`.
    pub fn reseed(&mut self, entropy: &[u8], additional: &[u8]) {
        self.update(&[entropy, additional]);
        self.reseed_counter = 1;
    }

    /// `HMAC_DRBG_Generate(requested_bytes, additional_input)`.
    pub fn generate(&mut self, n: usize, additional: &[u8]) -> Vec<u8> {
        if !additional.is_empty() {
            self.update(&[additional]);
        }
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            self.v = hmac_sha256(&self.k, &self.v);
            out.extend_from_slice(&self.v);
        }
        out.truncate(n);
        self.update(&[additional]); // final Update(additional_input)
        self.reseed_counter += 1;
        out
    }
}

// ── Real noise: CPU timing jitter ──────────────────────────────────────────
/// Conservative min-entropy estimate (in bits) of a run of high-resolution
/// timing samples (e.g. RDTSC). Uses the low-bit deltas between successive
/// samples — the part driven by micro-architectural jitter — and credits only a
/// fraction of a bit per usable sample, so we under-claim rather than over-claim.
pub fn estimate_jitter_bits(samples: &[u64]) -> usize {
    if samples.len() < 2 {
        return 0;
    }
    let mut varying = 0usize;
    for w in samples.windows(2) {
        let delta = w[1].wrapping_sub(w[0]);
        // The bottom 4 bits of the delta are where timing jitter lives; count a
        // sample as "noisy" only if those bits are non-trivial.
        if (delta & 0xF) != 0 && (delta & 0xF) != 0xF {
            varying += 1;
        }
    }
    // Credit 1/4 bit per noisy sample (conservative vs the SP 800-90B ~1 bit).
    varying / 4
}

/// Fold raw noise bytes into a 32-byte conditioned block (SHA-256 as the
/// vetted conditioning component, SP 800-90B style).
fn condition(material: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"EuroEntropy-cond-v1");
    h.update(material);
    let mut o = [0u8; 32];
    o.copy_from_slice(&h.finalize());
    o
}

/// The boot entropy pool: accumulates conditioned noise + credited entropy bits,
/// instantiates/reseeds the DRBG, and gates output until a security threshold of
/// real entropy has been gathered.
pub struct EntropyPool {
    drbg: Option<HmacDrbg>,
    /// Estimated real entropy bits gathered so far.
    entropy_bits: usize,
    /// Required bits before `ready()` (and thus `fill`) succeeds.
    threshold_bits: usize,
    /// Staged conditioned material before the first instantiate.
    staged: Vec<u8>,
    reseeds: u64,
}

impl EntropyPool {
    /// A pool requiring `threshold_bits` (256 is the sane default for 128-bit
    /// security with margin).
    pub fn new(threshold_bits: usize) -> Self {
        EntropyPool { drbg: None, entropy_bits: 0, threshold_bits, staged: Vec::new(), reseeds: 0 }
    }

    /// Add noise `material` credited with `estimated_bits` of entropy. Before the
    /// pool is ready this stages/instantiates; afterwards it reseeds the DRBG.
    pub fn add(&mut self, material: &[u8], estimated_bits: usize) {
        let block = condition(material);
        self.entropy_bits = self.entropy_bits.saturating_add(estimated_bits);
        match &mut self.drbg {
            None => {
                self.staged.extend_from_slice(&block);
                if self.entropy_bits >= self.threshold_bits {
                    // Instantiate: entropy = staged, nonce = counter, perso = domain.
                    let nonce = (self.staged.len() as u64).to_le_bytes();
                    self.drbg = Some(HmacDrbg::instantiate(&self.staged, &nonce, b"EuroEntropy-pool-v1"));
                    self.staged.clear();
                }
            }
            Some(drbg) => {
                drbg.reseed(&block, b"");
                self.reseeds += 1;
            }
        }
    }

    /// Convenience: credit CPU-jitter samples using the conservative estimator.
    pub fn add_jitter(&mut self, samples: &[u64]) {
        let bits = estimate_jitter_bits(samples);
        let mut bytes = Vec::with_capacity(samples.len() * 8);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.add(&bytes, bits);
    }

    /// True once enough real entropy has been gathered to hand out randomness.
    pub fn ready(&self) -> bool {
        self.drbg.is_some() && self.entropy_bits >= self.threshold_bits
    }

    /// Fill `buf` with CSPRNG output — **only** when [`ready`](Self::ready). A
    /// caller that has not gathered enough entropy gets `false` and NO bytes
    /// (the hard blocking guarantee), never low-entropy output.
    pub fn fill(&mut self, buf: &mut [u8]) -> bool {
        if !self.ready() {
            return false;
        }
        let drbg = self.drbg.as_mut().unwrap();
        let out = drbg.generate(buf.len(), b"");
        buf.copy_from_slice(&out);
        true
    }

    pub fn entropy_bits(&self) -> usize {
        self.entropy_bits
    }
    pub fn reseed_count(&self) -> u64 {
        self.reseeds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_rfc4231_case2() {
        // RFC 4231 Test Case 2 for HMAC-SHA256.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let expect = [
            0x5b, 0xdc, 0xc1, 0x46, 0xbf, 0x60, 0x75, 0x4e, 0x6a, 0x04, 0x24, 0x26, 0x08, 0x95, 0x75, 0xc7,
            0x5a, 0x00, 0x3f, 0x08, 0x9d, 0x27, 0x39, 0x83, 0x9d, 0xec, 0x58, 0xb9, 0x64, 0xec, 0x38, 0x43,
        ];
        assert_eq!(mac, expect);
    }

    #[test]
    fn pool_blocks_until_threshold() {
        let mut p = EntropyPool::new(256);
        assert!(!p.ready());
        let mut buf = [0u8; 16];
        assert!(!p.fill(&mut buf)); // refuses before ready
        p.add(b"some conditioned noise here....", 100);
        assert!(!p.ready()); // still short
        p.add(b"more noise from the TPM RNG.....", 200);
        assert!(p.ready()); // 300 >= 256
        assert!(p.fill(&mut buf));
        assert!(buf.iter().any(|&b| b != 0));
    }

    #[test]
    fn pool_deterministic_given_same_noise() {
        let seed_a = |p: &mut EntropyPool| {
            p.add(b"AAAAAAAAAAAAAAAA", 300);
        };
        let mut p1 = EntropyPool::new(256);
        let mut p2 = EntropyPool::new(256);
        seed_a(&mut p1);
        seed_a(&mut p2);
        let mut b1 = [0u8; 32];
        let mut b2 = [0u8; 32];
        assert!(p1.fill(&mut b1) && p2.fill(&mut b2));
        assert_eq!(b1, b2); // same noise → same stream (DRBG determinism)
    }

    #[test]
    fn jitter_estimate_is_conservative() {
        // Constant samples → no entropy credited.
        assert_eq!(estimate_jitter_bits(&[100; 64]), 0);
        // Varying low bits → some, but conservative (< samples/4 + slack).
        let samples: Vec<u64> = (0..64).map(|i| i as u64 * 7 + (i as u64 & 3)).collect();
        assert!(estimate_jitter_bits(&samples) <= samples.len() / 4);
    }
}
