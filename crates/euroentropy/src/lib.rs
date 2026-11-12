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
//! - [`EntropyPool::seed_from`] — the seeding procedure: which sources
//!   ([`NoiseSources`]: TPM, RDSEED, RDRAND, jitter), in which order, how much
//!   each is credited, and that it goes on gathering until the threshold is met
//!   or a bound is reached ([`SeedPolicy`], [`SeedReport`]).
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
    /// What [`seed_from`](Self::seed_from) has gathered so far, over all its calls.
    report: SeedReport,
}

/// Staged material is folded into one conditioned block when it reaches this
/// size, so a pool that waits long for its threshold does not grow without end.
const STAGED_FOLD_BYTES: usize = 32 * 64;

impl EntropyPool {
    /// A pool requiring `threshold_bits` (256 is the sane default for 128-bit
    /// security with margin).
    pub fn new(threshold_bits: usize) -> Self {
        EntropyPool {
            drbg: None,
            entropy_bits: 0,
            threshold_bits,
            staged: Vec::new(),
            reseeds: 0,
            report: SeedReport { threshold: threshold_bits, ..SeedReport::default() },
        }
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
                } else if self.staged.len() >= STAGED_FOLD_BYTES {
                    let folded = condition(&self.staged);
                    self.staged.clear();
                    self.staged.extend_from_slice(&folded);
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

// ── Seeding: which sources, in which order, until when ─────────────────────

/// What a platform can offer [`EntropyPool::seed_from`]. A source that is not
/// there answers `None`; the jitter source is the one every platform has.
pub trait NoiseSources {
    /// Up to 32 bytes from a TPM's random number generator.
    fn tpm(&mut self) -> Option<Vec<u8>> {
        None
    }
    /// One word from the CPU's entropy source (RDSEED), if the CPU has the
    /// instruction and it delivered.
    fn rdseed(&mut self) -> Option<u64> {
        None
    }
    /// One word from the CPU's generator (RDRAND), likewise.
    fn rdrand(&mut self) -> Option<u64> {
        None
    }
    /// `n` raw timing samples. An empty batch counts as a batch without credit.
    fn jitter(&mut self, n: usize) -> Vec<u64>;
}

/// Credit for one RDSEED word of 64 bits: half of what the CPU claims.
pub const RDSEED_BITS_PER_WORD: usize = 32;
/// Credit for one RDRAND word of 64 bits: a quarter (RDRAND is the output of
/// the CPU's own DRBG, reseeded from the same source as RDSEED).
pub const RDRAND_BITS_PER_WORD: usize = 16;
/// Credit for one byte from the TPM (32 bytes: 256 bits, as before).
pub const TPM_BITS_PER_BYTE: usize = 8;

/// How long [`EntropyPool::seed_from`] gathers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeedPolicy {
    /// Timing samples per batch.
    pub jitter_batch: usize,
    /// Batches of jitter that are gathered and mixed in whatever the other
    /// sources gave: no source replaces the jitter.
    pub min_jitter_batches: usize,
    /// The bound: after this many batches in one call the seeding gives up,
    /// and the pool stays closed.
    pub max_jitter_batches: usize,
    /// Words asked of RDSEED and of RDRAND per call.
    pub cpu_words: usize,
}

impl SeedPolicy {
    /// At boot: gather until the threshold is met, at most 4096 batches.
    pub const BOOT: SeedPolicy = SeedPolicy { jitter_batch: 512, min_jitter_batches: 4, max_jitter_batches: 4096, cpu_words: 8 };
    /// A later attempt on behalf of a request that found the pool closed.
    pub const RETRY: SeedPolicy = SeedPolicy { jitter_batch: 512, min_jitter_batches: 1, max_jitter_batches: 64, cpu_words: 8 };
}

/// What the seeding gathered (summed over every call on one pool).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// The pool hands out randomness.
    pub ready: bool,
    /// Credited entropy so far, and the threshold it has to reach.
    pub bits: usize,
    pub threshold: usize,
    pub tpm_bytes: usize,
    pub rdseed_words: usize,
    pub rdrand_words: usize,
    /// Credit given to the two CPU sources together (capped, see `seed_from`).
    pub cpu_bits: usize,
    pub jitter_batches: usize,
    pub jitter_bits: usize,
    /// The last call reached its bound without meeting the threshold.
    pub gave_up: bool,
}

impl EntropyPool {
    /// Seed the pool from every source the platform has, and go on gathering
    /// jitter until the pool is ready or `policy.max_jitter_batches` is reached.
    ///
    /// Order: the TPM (if any), RDSEED and RDRAND (if the CPU has them), then
    /// jitter. All of it is conditioned and mixed into the same seed; nothing
    /// replaces anything.
    ///
    /// Credit:
    /// - TPM bytes at [`TPM_BITS_PER_BYTE`], once per pool;
    /// - RDSEED and RDRAND words at [`RDSEED_BITS_PER_WORD`] and
    ///   [`RDRAND_BITS_PER_WORD`], together **at most half the threshold**: the
    ///   CPU's generator alone never opens the pool, the other half has to
    ///   come from jitter or the TPM;
    /// - jitter by [`estimate_jitter_bits`], unchanged.
    ///
    /// When the bound is reached the pool stays closed (`gave_up`, and
    /// [`fill`](Self::fill) refuses); what was gathered stays staged, so a
    /// later call continues from there.
    pub fn seed_from(&mut self, src: &mut dyn NoiseSources, policy: &SeedPolicy) -> SeedReport {
        // 1. TPM.
        if let Some(b) = src.tpm() {
            if !b.is_empty() {
                let credit = if self.report.tpm_bytes == 0 { b.len().min(32) * TPM_BITS_PER_BYTE } else { 0 };
                self.add(&b, credit);
                self.report.tpm_bytes += b.len();
            }
        }
        // 2. The CPU's sources. Material from both goes in; the credit is capped.
        let cpu_cap = self.threshold_bits / 2;
        let mut words: Vec<u8> = Vec::new();
        let mut credit = 0usize;
        for _ in 0..policy.cpu_words {
            if let Some(w) = src.rdseed() {
                words.extend_from_slice(&w.to_le_bytes());
                self.report.rdseed_words += 1;
                credit += RDSEED_BITS_PER_WORD;
            }
        }
        for _ in 0..policy.cpu_words {
            if let Some(w) = src.rdrand() {
                words.extend_from_slice(&w.to_le_bytes());
                self.report.rdrand_words += 1;
                credit += RDRAND_BITS_PER_WORD;
            }
        }
        if !words.is_empty() {
            let credit = credit.min(cpu_cap.saturating_sub(self.report.cpu_bits));
            self.add(&words, credit);
            self.report.cpu_bits += credit;
        }
        // 3. Jitter: at least the minimum, then until ready, at most the bound.
        let mut batches = 0usize;
        self.report.gave_up = false;
        loop {
            if batches >= policy.min_jitter_batches && self.ready() {
                break;
            }
            if batches >= policy.max_jitter_batches {
                self.report.gave_up = !self.ready();
                break;
            }
            let samples = src.jitter(policy.jitter_batch);
            self.report.jitter_bits += estimate_jitter_bits(&samples);
            self.add_jitter(&samples);
            self.report.jitter_batches += 1;
            batches += 1;
        }
        self.report.ready = self.ready();
        self.report.bits = self.entropy_bits;
        self.report
    }

    /// What [`seed_from`](Self::seed_from) has gathered so far.
    pub fn seed_report(&self) -> SeedReport {
        SeedReport { ready: self.ready(), bits: self.entropy_bits, ..self.report }
    }
}

#[cfg(test)]
mod seed_tests {
    use super::*;

    /// A scripted platform: `quiet` batches of constant samples (no credit),
    /// then noisy ones; optional CPU sources and TPM.
    struct Script {
        quiet: usize,
        asked: usize,
        rdseed: bool,
        rdrand: bool,
        tpm: bool,
        x: u64,
    }
    impl Script {
        fn new(quiet: usize) -> Self {
            Script { quiet, asked: 0, rdseed: false, rdrand: false, tpm: false, x: 0x1234_5678_9abc_def1 }
        }
        fn next(&mut self) -> u64 {
            // xorshift64: test data, not randomness.
            self.x ^= self.x << 13;
            self.x ^= self.x >> 7;
            self.x ^= self.x << 17;
            self.x
        }
    }
    impl NoiseSources for Script {
        fn tpm(&mut self) -> Option<Vec<u8>> {
            self.tpm.then(|| vec![0xA5; 32])
        }
        fn rdseed(&mut self) -> Option<u64> {
            if self.rdseed {
                Some(self.next())
            } else {
                None
            }
        }
        fn rdrand(&mut self) -> Option<u64> {
            if self.rdrand {
                Some(self.next())
            } else {
                None
            }
        }
        fn jitter(&mut self, n: usize) -> Vec<u64> {
            self.asked += 1;
            if self.asked <= self.quiet {
                vec![24; n]
            } else {
                (0..n).map(|_| 100 + (self.next() & 0x3f)).collect()
            }
        }
    }

    const P: SeedPolicy = SeedPolicy { jitter_batch: 512, min_jitter_batches: 2, max_jitter_batches: 100, cpu_words: 8 };

    #[test]
    fn a_quiet_start_is_waited_out() {
        // The BUG-065 boot: the first batches carry nothing. The old init
        // stopped after a fixed number; this one goes on until there is enough.
        let mut src = Script::new(70);
        let mut p = EntropyPool::new(256);
        let r = p.seed_from(&mut src, &P);
        assert!(r.ready && !r.gave_up);
        assert!(r.jitter_batches > 70 && r.jitter_batches < 100);
        assert!(r.jitter_bits >= 256 && r.bits >= 256);
        let mut b = [0u8; 32];
        assert!(p.fill(&mut b));
    }

    #[test]
    fn no_noise_ends_at_the_bound_and_the_pool_stays_closed() {
        let mut src = Script::new(usize::MAX);
        let mut p = EntropyPool::new(256);
        let r = p.seed_from(&mut src, &P);
        assert!(!r.ready && r.gave_up);
        assert_eq!(r.jitter_batches, 100);
        assert_eq!(r.bits, 0);
        let mut b = [0x77u8; 32];
        assert!(!p.fill(&mut b));
        assert_eq!(b, [0x77u8; 32]); // not a byte written
    }

    #[test]
    fn a_later_attempt_seeds_a_pool_that_gave_up() {
        let mut src = Script::new(100);
        let mut p = EntropyPool::new(256);
        assert!(p.seed_from(&mut src, &P).gave_up);
        // The same platform, later: the noise is there now.
        let r = p.seed_from(&mut src, &SeedPolicy::RETRY);
        assert!(r.ready && !r.gave_up);
        assert_eq!(r.jitter_batches, p.seed_report().jitter_batches);
        assert!(r.jitter_batches > 100);
        let mut b = [0u8; 16];
        assert!(p.fill(&mut b));
    }

    #[test]
    fn the_cpu_sources_alone_never_open_the_pool() {
        let mut src = Script::new(usize::MAX);
        src.rdseed = true;
        src.rdrand = true;
        let mut p = EntropyPool::new(256);
        let r = p.seed_from(&mut src, &P);
        assert_eq!((r.rdseed_words, r.rdrand_words), (8, 8));
        assert_eq!(r.cpu_bits, 128); // 8 x 32 + 8 x 16 = 384, capped at half of 256
        assert!(!r.ready && r.gave_up);
        // Asking again does not add credit past the cap.
        let r = p.seed_from(&mut src, &P);
        assert_eq!(r.cpu_bits, 128);
        assert_eq!(r.bits, 128);
        assert!(!r.ready);
    }

    #[test]
    fn rdrand_alone_is_credited_less_than_rdseed() {
        let mut src = Script::new(usize::MAX);
        src.rdrand = true;
        let mut p = EntropyPool::new(256);
        let r = p.seed_from(&mut src, &SeedPolicy { cpu_words: 4, max_jitter_batches: 1, ..P });
        assert_eq!((r.rdseed_words, r.rdrand_words, r.cpu_bits), (0, 4, 64));
    }

    #[test]
    fn cpu_words_and_jitter_are_mixed_not_swapped() {
        // The same jitter with and without the CPU's words: both pools open,
        // and they give different streams, so the words are in the seed.
        let mut a = Script::new(0);
        let mut b = Script::new(0);
        b.rdseed = true;
        let mut pa = EntropyPool::new(256);
        let mut pb = EntropyPool::new(256);
        let ra = pa.seed_from(&mut a, &P);
        let rb = pb.seed_from(&mut b, &P);
        assert!(ra.ready && rb.ready);
        assert!(rb.jitter_batches >= P.min_jitter_batches && rb.jitter_bits > 0);
        assert_eq!(rb.rdseed_words, 8);
        let (mut oa, mut ob) = ([0u8; 32], [0u8; 32]);
        assert!(pa.fill(&mut oa) && pb.fill(&mut ob));
        assert_ne!(oa, ob);
    }

    #[test]
    fn jitter_is_gathered_even_when_the_tpm_alone_meets_the_threshold() {
        let mut src = Script::new(0);
        src.tpm = true;
        let mut p = EntropyPool::new(256);
        let r = p.seed_from(&mut src, &P);
        assert!(r.ready);
        assert_eq!(r.tpm_bytes, 32);
        assert_eq!(r.jitter_batches, P.min_jitter_batches);
        // A second call does not credit the TPM again.
        let before = p.entropy_bits();
        let r2 = p.seed_from(&mut src, &SeedPolicy { min_jitter_batches: 0, ..P });
        assert_eq!(p.entropy_bits(), before);
        assert_eq!(r2.tpm_bytes, 64);
    }

    #[test]
    fn a_long_wait_does_not_grow_the_staged_material() {
        let mut p = EntropyPool::new(256);
        for i in 0..1000u32 {
            p.add(&i.to_le_bytes(), 0);
        }
        assert!(p.staged.len() <= STAGED_FOLD_BYTES);
        assert!(!p.ready());
        p.add(b"enough", 256);
        assert!(p.ready());
    }

    #[test]
    fn the_estimator_gives_a_constant_series_nothing() {
        // What the old two-instruction sample looked like in a BUG-065 boot.
        assert_eq!(estimate_jitter_bits(&[24; 512]), 0);
        let mut s = vec![24u64; 512];
        for i in (8..512).step_by(9) {
            s[i] = 28;
        }
        // 56 steps up of 4 are counted, the 56 steps back are too: 112 / 4.
        assert!(estimate_jitter_bits(&s) <= 28);
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
