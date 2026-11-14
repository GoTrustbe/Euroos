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

// ── SP 800-90B health tests (BUG-082) ───────────────────────────────────────
//
// The two continuous tests of SP 800-90B section 4.4 on the raw samples of a
// noise source, before anything is conditioned or credited:
//
// - the repetition count test: C or more identical samples in a row fail,
//   C = 1 + ceil(20 / H) for a false-positive rate of 2^-20;
// - the adaptive proportion test: in a window of W samples, the first sample
//   of the window occurring C or more times fails, C = 1 + the (1 - 2^-20)
//   quantile of Binomial(W, 2^-H).
//
// H is the min-entropy per sample the cutoffs assume. For the jitter sample it
// is 1 bit: below what the memory walk measured (the most frequent value is 5
// to 19 % of a batch, 2.4 bits and up), and above what is credited (a quarter
// bit per counted sample), so a source that falls below one bit per sample is
// caught, while a healthy one fails once in a million windows. The old
// two-instruction sample (one value in about 70 % of a batch) fails the
// proportion test; a constant one fails the repetition test.
//
// A failed jitter batch is discarded: its samples are neither mixed nor
// credited, and the next batch is tested afresh. A CPU word source (RDSEED,
// RDRAND) or the TPM that fails is disabled for the life of the pool.

/// Assumed min-entropy of one jitter sample, in bits, for the cutoffs below.
pub const JITTER_H_BITS: u32 = 1;
/// Repetition count cutoff for H = 1: 1 + ceil(20 / 1).
pub const JITTER_RCT_CUTOFF: u32 = 21;
/// Adaptive proportion window (SP 800-90B: 512 for a non-binary source).
pub const APT_WINDOW: usize = 512;
/// Adaptive proportion cutoff for H = 1 and W = 512 (SP 800-90B, table 2).
pub const JITTER_APT_CUTOFF: u32 = 311;
/// Repetition count cutoff for a 64-bit CPU word credited at 32 bits (H = 32):
/// 1 + ceil(20 / 32) = 2, two equal words in a row.
pub const WORD_RCT_CUTOFF: u32 = 2;
/// Repetition count cutoff for TPM bytes, assumed H = 4: 1 + ceil(20 / 4).
pub const TPM_RCT_CUTOFF: u32 = 6;

/// Which test failed, on which value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthFailure {
    /// `run` equal samples in a row (`run >= cutoff`).
    Repetition { value: u64, run: u32, cutoff: u32 },
    /// `count` of the window's `window` samples equal to its first one.
    Proportion { value: u64, count: u32, window: u32, cutoff: u32 },
}

impl HealthFailure {
    /// "repetition count 21 >= 21 (value 24)" and the like.
    pub fn describe(&self) -> alloc::string::String {
        match *self {
            HealthFailure::Repetition { value, run, cutoff } => {
                alloc::format!("repetition count {run} >= cutoff {cutoff} (value {value:#x})")
            }
            HealthFailure::Proportion { value, count, window, cutoff } => {
                alloc::format!("adaptive proportion {count} of {window} >= cutoff {cutoff} (value {value:#x})")
            }
        }
    }
}

/// The repetition count test over `samples`, continuing from `state`
/// (the last value and its run length).
fn rct(samples: &[u64], cutoff: u32, state: &mut Option<(u64, u32)>) -> Result<(), HealthFailure> {
    for &x in samples {
        let next = match *state {
            Some((v, run)) if v == x => (v, run + 1),
            _ => (x, 1),
        };
        *state = Some(next);
        if next.1 >= cutoff {
            return Err(HealthFailure::Repetition { value: x, run: next.1, cutoff });
        }
    }
    Ok(())
}

/// The adaptive proportion test over whole windows of `samples` (a shorter
/// tail is a window of its own).
fn apt(samples: &[u64], window: usize, cutoff: u32) -> Result<(), HealthFailure> {
    for w in samples.chunks(window) {
        let first = w[0];
        let count = w.iter().filter(|&&x| x == first).count() as u32;
        if count >= cutoff {
            return Err(HealthFailure::Proportion { value: first, count, window: w.len() as u32, cutoff });
        }
    }
    Ok(())
}

/// Both tests on one batch of jitter samples (a batch is tested on its own:
/// batches are minutes or microseconds apart, a run does not carry over).
pub fn jitter_health(samples: &[u64]) -> Result<(), HealthFailure> {
    let mut st = None;
    rct(samples, JITTER_RCT_CUTOFF, &mut st)?;
    apt(samples, APT_WINDOW, JITTER_APT_CUTOFF)
}

/// The continuous state of the sources other than jitter.
#[derive(Clone, Copy, Debug, Default)]
struct SourceHealth {
    rdseed: Option<(u64, u32)>,
    rdrand: Option<(u64, u32)>,
    tpm: Option<(u64, u32)>,
    rdseed_off: bool,
    rdrand_off: bool,
    tpm_off: bool,
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
    /// The health state of the TPM and the CPU sources (BUG-082).
    health: SourceHealth,
    reseeds_requested: u64,
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
            health: SourceHealth::default(),
            reseeds_requested: 0,
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
    /// Health test failures (BUG-082), all sources, and the last one.
    pub health_failures: usize,
    pub last_failure: Option<(HealthSource, HealthFailure)>,
    /// Jitter batches the health tests discarded (not mixed, not credited).
    pub jitter_discarded: usize,
    /// Sources the health tests switched off for the life of the pool.
    pub rdseed_off: bool,
    pub rdrand_off: bool,
    pub tpm_off: bool,
}

/// A noise source, as the health report names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthSource {
    Jitter,
    Rdseed,
    Rdrand,
    Tpm,
}

impl HealthSource {
    pub fn name(self) -> &'static str {
        match self {
            HealthSource::Jitter => "jitter",
            HealthSource::Rdseed => "rdseed",
            HealthSource::Rdrand => "rdrand",
            HealthSource::Tpm => "tpm",
        }
    }
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
        if !self.health.tpm_off {
            if let Some(b) = src.tpm() {
                if !b.is_empty() {
                    let raw: Vec<u64> = b.iter().map(|&x| x as u64).collect();
                    match rct(&raw, TPM_RCT_CUTOFF, &mut self.health.tpm) {
                        Ok(()) => {
                            let credit = if self.report.tpm_bytes == 0 { b.len().min(32) * TPM_BITS_PER_BYTE } else { 0 };
                            self.add(&b, credit);
                            self.report.tpm_bytes += b.len();
                        }
                        Err(f) => {
                            self.health.tpm_off = true;
                            self.health_failed(HealthSource::Tpm, f);
                        }
                    }
                }
            }
        }
        // 2. The CPU's sources. Material from both goes in; the credit is capped.
        let cpu_cap = self.threshold_bits / 2;
        let mut words: Vec<u8> = Vec::new();
        let mut credit = 0usize;
        for (source, per_word) in [(HealthSource::Rdseed, RDSEED_BITS_PER_WORD), (HealthSource::Rdrand, RDRAND_BITS_PER_WORD)] {
            let mut got: Vec<u64> = Vec::new();
            for _ in 0..policy.cpu_words {
                let off = if source == HealthSource::Rdseed { self.health.rdseed_off } else { self.health.rdrand_off };
                if off {
                    break;
                }
                let w = if source == HealthSource::Rdseed { src.rdseed() } else { src.rdrand() };
                let Some(w) = w else { continue };
                let st = if source == HealthSource::Rdseed { &mut self.health.rdseed } else { &mut self.health.rdrand };
                if let Err(f) = rct(&[w], WORD_RCT_CUTOFF, st) {
                    // A stuck generator: none of this call's words are used.
                    got.clear();
                    if source == HealthSource::Rdseed {
                        self.health.rdseed_off = true;
                    } else {
                        self.health.rdrand_off = true;
                    }
                    self.health_failed(source, f);
                    break;
                }
                got.push(w);
            }
            for w in &got {
                words.extend_from_slice(&w.to_le_bytes());
                credit += per_word;
            }
            if source == HealthSource::Rdseed {
                self.report.rdseed_words += got.len();
            } else {
                self.report.rdrand_words += got.len();
            }
        }
        if !words.is_empty() {
            let credit = credit.min(cpu_cap.saturating_sub(self.report.cpu_bits));
            self.add(&words, credit);
            self.report.cpu_bits += credit;
        }
        // 3. Jitter: at least the minimum, then until ready, at most the bound.
        //    A batch that fails a health test is discarded.
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
            self.report.jitter_batches += 1;
            batches += 1;
            if samples.is_empty() {
                continue;
            }
            if let Err(f) = jitter_health(&samples) {
                self.report.jitter_discarded += 1;
                self.health_failed(HealthSource::Jitter, f);
                continue;
            }
            self.report.jitter_bits += estimate_jitter_bits(&samples);
            self.add_jitter(&samples);
        }
        self.report.ready = self.ready();
        self.report.bits = self.entropy_bits;
        self.seed_report()
    }

    /// What [`seed_from`](Self::seed_from) has gathered so far.
    pub fn seed_report(&self) -> SeedReport {
        SeedReport {
            ready: self.ready(),
            bits: self.entropy_bits,
            rdseed_off: self.health.rdseed_off,
            rdrand_off: self.health.rdrand_off,
            tpm_off: self.health.tpm_off,
            ..self.report
        }
    }

    fn health_failed(&mut self, source: HealthSource, f: HealthFailure) {
        self.report.health_failures += 1;
        self.report.last_failure = Some((source, f));
    }

    /// Reseed a seeded pool from fresh noise (BUG-089: after the restore of a
    /// snapshot, in a clone, or because it is time): the CPU words and
    /// `batches` jitter batches, health-tested as at boot, and `additional`
    /// (what tells this instance apart: a VM generation id, the clock) mixed
    /// in whatever the sources gave. The report of the pool afterwards.
    pub fn reseed_from(&mut self, src: &mut dyn NoiseSources, batches: usize, additional: &[u8]) -> SeedReport {
        let policy = SeedPolicy { jitter_batch: 512, min_jitter_batches: batches, max_jitter_batches: batches, cpu_words: 8 };
        self.seed_from(src, &policy);
        if !additional.is_empty() {
            self.add(additional, 0);
        }
        self.reseeds_requested += 1;
        self.seed_report()
    }

    /// Reseeds asked for through [`reseed_from`](Self::reseed_from).
    pub fn reseeds_requested(&self) -> u64 {
        self.reseeds_requested
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
            // Varying bytes: a TPM that hands out one byte value fails its health test.
            self.tpm.then(|| (0..32u8).map(|i| i.wrapping_mul(37) ^ 0xA5).collect())
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

#[cfg(test)]
mod health_tests {
    //! BUG-082: the SP 800-90B health tests against models of a source.
    use super::*;

    /// xorshift64*: test data, not randomness.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        /// A value of a source whose most frequent value has probability
        /// `p_top` per mille and is otherwise spread over 64 values.
        fn sample(&mut self, p_top: u64) -> u64 {
            let r = self.next();
            if r % 1000 < p_top { 100 } else { 101 + (r >> 20) % 64 }
        }
    }

    /// The memory-walk sample as measured: 88 ticks and up, many values, the
    /// most frequent one at 5 to 19 % of a batch (proof/integ-2026-11b).
    fn walk_batch(rng: &mut Rng) -> Vec<u64> {
        (0..512).map(|_| if rng.next() % 100 < 19 { 120 } else { 88 + rng.next() % 140 }).collect()
    }

    #[test]
    fn the_cutoffs_are_the_sp800_90b_ones() {
        // RCT: C = 1 + ceil(20 / H).
        assert_eq!(JITTER_RCT_CUTOFF, 1 + (20 + JITTER_H_BITS - 1) / JITTER_H_BITS);
        assert_eq!(WORD_RCT_CUTOFF, 1 + (20 + 32 - 1) / 32);
        assert_eq!(TPM_RCT_CUTOFF, 1 + (20 + 4 - 1) / 4);
        // APT: C = 1 + the (1 - 2^-20) quantile of Binomial(512, 2^-H), H = 1,
        // i.e. the smallest c with P(X >= c) <= 2^-20.
        let n = APT_WINDOW as u32;
        let p = 0.5f64;
        let alpha = 2f64.powi(-20);
        let ln_choose = |k: u32| -> f64 { (0..k).map(|i| ((n - i) as f64).ln() - ((i + 1) as f64).ln()).sum() };
        let tail = |c: u32| -> f64 {
            (c..=n).map(|k| (ln_choose(k) + k as f64 * p.ln() + (n - k) as f64 * (1.0 - p).ln()).exp()).sum()
        };
        let c = (1..=n).find(|&c| tail(c) <= alpha).unwrap();
        assert_eq!(JITTER_APT_CUTOFF, c, "computed cutoff {c}");
    }

    #[test]
    fn a_constant_source_fails_the_repetition_test() {
        let e = jitter_health(&[24; 512]).unwrap_err();
        assert_eq!(e, HealthFailure::Repetition { value: 24, run: 21, cutoff: 21 });
        // 20 in a row is still allowed.
        let mut s: Vec<u64> = (0..512).map(|i| i as u64).collect();
        for x in s.iter_mut().take(20) {
            *x = 7;
        }
        assert!(jitter_health(&s).is_ok());
    }

    #[test]
    fn the_old_periodic_sample_fails_the_proportion_test_and_was_credited() {
        // BUG-082: the two-instruction sample of the BUG-065 diagnosis,
        // 12, 12, 12, 12, 38, 12, 12, 12, 14, 38, ... The estimator credits it.
        let pat = [12u64, 12, 12, 12, 38, 12, 12, 12, 14, 38];
        let s: Vec<u64> = (0..512).map(|i| pat[i % pat.len()]).collect();
        assert!(estimate_jitter_bits(&s) >= 60, "the estimator alone gives it {}", estimate_jitter_bits(&s));
        match jitter_health(&s) {
            Err(HealthFailure::Proportion { value: 12, count, window: 512, cutoff: 311 }) => assert!(count >= 311),
            other => panic!("not refused by the proportion test: {other:?}"),
        }
    }

    #[test]
    fn a_source_of_a_handful_of_values_fails() {
        // Two values, one of them in 75 % of the samples (under 1 bit/sample).
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let s: Vec<u64> = (0..512).map(|_| if rng.next() % 4 == 0 { 38 } else { 12 }).collect();
        assert!(jitter_health(&s).is_err());
    }

    #[test]
    fn healthy_models_pass_twenty_thousand_batches() {
        // The measured walk and a source at 2 bits per sample fail none of
        // 20 000 batches. A source at exactly the 1 bit the cutoffs assume
        // (most frequent value 1/2) is the edge: the repetition test has a
        // false-positive rate of 2^-20 per sample, 512 x 2^-21 per batch, about
        // 5 in 20 000 batches; it may fail a few, never many.
        let mut rng = Rng(0x0123_4567_89AB_CDEF);
        for _ in 0..20_000 {
            assert!(jitter_health(&walk_batch(&mut rng)).is_ok());
        }
        for (p_top, most) in [(250u64, 0u32), (500, 20)] {
            let mut fails = 0;
            for _ in 0..20_000 {
                let s: Vec<u64> = (0..512).map(|_| rng.sample(p_top)).collect();
                fails += jitter_health(&s).is_err() as u32;
            }
            assert!(fails <= most, "p_top {p_top} per mille: {fails} of 20 000 batches failed");
        }
    }

    #[test]
    fn a_source_below_one_bit_is_caught() {
        // Most frequent value at 70 %: about 0.5 bit per sample. The
        // proportion test counts the window's FIRST value, so it catches the
        // 70 % of windows that start with the frequent one (358 of 512 on
        // average, the cutoff is 311); the repetition test some of the rest.
        let mut rng = Rng(42);
        let mut fails = 0;
        for _ in 0..1000 {
            let s: Vec<u64> = (0..512).map(|_| rng.sample(700)).collect();
            fails += jitter_health(&s).is_err() as u32;
        }
        assert!(fails >= 650, "{fails} of 1000");
    }

    /// A platform with a scripted fault.
    struct Faulty {
        rng: Rng,
        stuck_rdrand: bool,
        stuck_tpm: bool,
        dead_jitter: bool,
    }
    impl NoiseSources for Faulty {
        fn tpm(&mut self) -> Option<Vec<u8>> {
            Some(if self.stuck_tpm { vec![0xFF; 32] } else { (0..32).map(|_| self.rng.next() as u8).collect() })
        }
        fn rdrand(&mut self) -> Option<u64> {
            // The AMD failure: all ones, every time.
            Some(if self.stuck_rdrand { u64::MAX } else { self.rng.next() })
        }
        fn jitter(&mut self, n: usize) -> Vec<u64> {
            if self.dead_jitter {
                vec![24; n]
            } else {
                walk_batch(&mut self.rng)[..n].to_vec()
            }
        }
    }
    fn faulty() -> Faulty {
        Faulty { rng: Rng(7), stuck_rdrand: false, stuck_tpm: false, dead_jitter: false }
    }
    const P: SeedPolicy = SeedPolicy { jitter_batch: 512, min_jitter_batches: 4, max_jitter_batches: 64, cpu_words: 8 };

    #[test]
    fn a_stuck_rdrand_is_switched_off_and_the_pool_seeds_from_the_rest() {
        let mut src = faulty();
        src.stuck_rdrand = true;
        let mut p = EntropyPool::new(256);
        let r = p.seed_from(&mut src, &P);
        assert!(r.rdrand_off && !r.tpm_off && !r.rdseed_off);
        assert_eq!(r.rdrand_words, 0, "no word of the stuck generator is used");
        assert!(matches!(
            r.last_failure,
            Some((HealthSource::Rdrand, HealthFailure::Repetition { value: u64::MAX, run: 2, cutoff: 2 }))
        ));
        assert!(r.ready, "jitter and the TPM still seed the pool");
        let r2 = p.seed_from(&mut src, &P);
        assert_eq!(r2.rdrand_words, 0, "not asked again");
        assert_eq!(r2.health_failures, 1);
    }

    #[test]
    fn a_stuck_tpm_is_switched_off() {
        let mut src = faulty();
        src.stuck_tpm = true;
        let mut p = EntropyPool::new(256);
        let r = p.seed_from(&mut src, &P);
        assert!(r.tpm_off);
        assert_eq!(r.tpm_bytes, 0);
        assert!(r.ready);
    }

    #[test]
    fn dead_jitter_is_discarded_and_the_pool_stays_closed_without_a_panic() {
        let mut src = faulty();
        src.dead_jitter = true;
        src.stuck_tpm = true;
        src.stuck_rdrand = true;
        let mut p = EntropyPool::new(256);
        let r = p.seed_from(&mut src, &P);
        assert!(!r.ready && r.gave_up);
        assert_eq!(r.jitter_discarded, 64);
        assert_eq!(r.jitter_bits, 0);
        let mut b = [0x33u8; 8];
        assert!(!p.fill(&mut b));
        assert_eq!(b, [0x33u8; 8]);
        // The source recovers: the next attempt tests afresh and seeds.
        src.dead_jitter = false;
        let r = p.seed_from(&mut src, &P);
        assert!(r.ready);
    }

    #[test]
    fn a_reseed_changes_the_stream_of_two_equal_pools() {
        // BUG-089: two pools in the same state (a snapshot restored twice) give
        // the same bytes until each is reseeded with what tells them apart.
        let mut a = EntropyPool::new(256);
        a.add(b"the same seed in both copies....", 300);
        let mut b = EntropyPool::new(256);
        b.add(b"the same seed in both copies....", 300);
        let (mut oa, mut ob) = ([0u8; 32], [0u8; 32]);
        assert!(a.fill(&mut oa) && b.fill(&mut ob));
        assert_eq!(oa, ob);
        // Even with the same noise: the generation ids differ.
        let mut sa = faulty();
        let mut sb = faulty();
        a.reseed_from(&mut sa, 1, b"vmgenid 1111");
        b.reseed_from(&mut sb, 1, b"vmgenid 2222");
        assert!(a.fill(&mut oa) && b.fill(&mut ob));
        assert_ne!(oa, ob);
        assert_eq!(a.reseeds_requested(), 1);
    }
}
