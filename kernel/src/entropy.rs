//! 3D-8 — the kernel early-boot CSPRNG. Gathers real noise (CPU timing **jitter**,
//! RDSEED and RDRAND when the CPU has them, the TPM RNG when there is one),
//! seeds an HMAC-DRBG ([`euroentropy`]), and hands out randomness **only** once
//! a security threshold of real entropy is reached. This closes the "weak
//! randomness at the worst moment" gap for FDE, TPM sealing, ML-KEM key
//! generation, the VPN and EuroCA — all of which draw key material very early.
//!
//! Seeding (BUG-065): [`init`] gathers until the pool has its 256 bits, with a
//! bound ([`SeedPolicy::BOOT`]); the sources are mixed, none replaces another;
//! the result is the `[entropy]` line every image prints. A pool that did not
//! get there stays closed: [`getrandom`] and [`draw`] refuse (after one more
//! bounded attempt to seed), and every caller has to deal with that refusal.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use euroentropy::{estimate_jitter_bits, EntropyPool, NoiseSources, SeedPolicy, SeedReport};
use spin::Mutex;

static POOL: Mutex<Option<EntropyPool>> = Mutex::new(None);
/// Requests refused because the pool was not seeded.
static REFUSED: AtomicU64 = AtomicU64::new(0);
/// Timer tick of the last seeding attempt made on behalf of a request.
static LAST_RETRY_TICK: AtomicU64 = AtomicU64::new(0);

#[inline]
fn rdtsc() -> u64 {
    // SAFETY: RDTSC is always available on x86-64 and has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// The block the jitter sample walks: larger than a core's first- and
/// second-level cache, so where an access is served from differs from access
/// to access.
const WALK_BYTES: usize = 512 * 1024;
/// Memory accesses between the two timestamps of one sample.
const WALK_STEPS: usize = 32;

/// The noise sources of this machine.
struct Platform {
    walk: Vec<u8>,
    acc: u64,
    rdseed: bool,
    rdrand: bool,
    tpm: bool,
}

impl Platform {
    /// Every source the machine has.
    fn all() -> Self {
        let (rdrand, rdseed) = cpu_rng_features();
        Platform { walk: alloc::vec![0u8; WALK_BYTES], acc: rdtsc() | 1, rdseed, rdrand, tpm: true }
    }
    /// The jitter source alone (the self-test's "no device, no CPU generator").
    fn jitter_only() -> Self {
        Platform { walk: alloc::vec![0u8; WALK_BYTES], acc: rdtsc() | 1, rdseed: false, rdrand: false, tpm: false }
    }
}

/// (RDRAND, RDSEED) as CPUID reports them. Without the bit the instruction is
/// an invalid opcode (qemu64 under TCG, `-cpu …,-rdrand,-rdseed`).
fn cpu_rng_features() -> (bool, bool) {
    // SAFETY: CPUID is available on every x86-64 CPU.
    unsafe {
        let max = core::arch::x86_64::__cpuid(0).eax;
        let rdrand = core::arch::x86_64::__cpuid(1).ecx & (1 << 30) != 0;
        let rdseed = max >= 7 && core::arch::x86_64::__cpuid_count(7, 0).ebx & (1 << 18) != 0;
        (rdrand, rdseed)
    }
}

impl NoiseSources for Platform {
    fn tpm(&mut self) -> Option<Vec<u8>> {
        if self.tpm {
            crate::tpm::get_random(32)
        } else {
            None
        }
    }

    fn rdseed(&mut self) -> Option<u64> {
        if !self.rdseed {
            return None;
        }
        // RDSEED may have nothing ready (CF = 0): try again a few times.
        for _ in 0..64 {
            let v: u64;
            let ok: u8;
            // SAFETY: the CPU reports RDSEED (checked in `cpu_rng_features`).
            unsafe {
                core::arch::asm!("rdseed {0}", "setc {1}", out(reg) v, out(reg_byte) ok, options(nomem, nostack));
            }
            if ok == 1 {
                return Some(v);
            }
            core::hint::spin_loop();
        }
        None
    }

    fn rdrand(&mut self) -> Option<u64> {
        if !self.rdrand {
            return None;
        }
        for _ in 0..16 {
            let v: u64;
            let ok: u8;
            // SAFETY: the CPU reports RDRAND (checked in `cpu_rng_features`).
            unsafe {
                core::arch::asm!("rdrand {0}", "setc {1}", out(reg) v, out(reg_byte) ok, options(nomem, nostack));
            }
            if ok == 1 {
                return Some(v);
            }
        }
        None
    }

    /// `n` timing samples: the TSC around [`WALK_STEPS`] read-modify-write
    /// accesses at pseudo-random places of a block that does not fit the
    /// near caches. How long that takes depends on where each line is at that
    /// moment (L1, L2, L3, the TLB), on the prefetchers and on what else the
    /// core and its sibling are doing. The duration of each sample is fed
    /// back into the walk, so the places depend on the timing before.
    ///
    /// Before BUG-065 a sample timed one multiplication between two RDTSCs:
    /// about 20 cycles. What varied there was the beat between the core's
    /// clock and the TSC's: a short repeating pattern in most P-states, and a
    /// constant in the P-states where the span is a whole number of TSC ticks
    /// (measured on the R630: 24, 24, 24, … for whole batches). The estimator
    /// rightly credited such batches nothing, and the old `init` then gave up.
    fn jitter(&mut self, n: usize) -> Vec<u64> {
        let mut s = Vec::with_capacity(n);
        let mask = self.walk.len() - 1; // WALK_BYTES is a power of two
        for _ in 0..n {
            let t0 = rdtsc();
            for _ in 0..WALK_STEPS {
                self.acc = self.acc.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let i = ((self.acc >> 33) as usize) & mask;
                self.walk[i] = self.walk[i].wrapping_add(self.acc as u8);
            }
            core::hint::black_box(&self.walk);
            let d = rdtsc().wrapping_sub(t0);
            self.acc ^= d;
            s.push(d);
        }
        s
    }
}

fn now_us() -> u64 {
    if crate::hpet::present() {
        crate::hpet::us()
    } else {
        crate::interrupts::ticks() * 10_000
    }
}

fn status_line(r: &SeedReport, us: u64) -> alloc::string::String {
    let tpm = if r.tpm_bytes > 0 { alloc::format!("{} bytes", r.tpm_bytes) } else { alloc::string::String::from("none") };
    let cpu = |have: bool, words: usize| -> alloc::string::String {
        if !have {
            alloc::string::String::from("not in this CPU")
        } else {
            alloc::format!("{words} words")
        }
    };
    let (have_rdrand, have_rdseed) = cpu_rng_features();
    let sources = alloc::format!(
        "tpm={tpm}, rdseed={}, rdrand={} (CPU credit {} bits, at most half the threshold), jitter={} batches of 512 samples ({} bits)",
        cpu(have_rdseed, r.rdseed_words),
        cpu(have_rdrand, r.rdrand_words),
        r.cpu_bits,
        r.jitter_batches,
        r.jitter_bits
    );
    if r.ready {
        alloc::format!(
            "[entropy] CSPRNG seeded: {} of {} bits · sources mixed: {sources} · {}.{:03} ms → OK",
            r.bits,
            r.threshold,
            us / 1000,
            us % 1000
        )
    } else {
        alloc::format!(
            "[entropy] CSPRNG NOT SEEDED: {} of {} bits · sources: {sources} · {}.{:03} ms → FAILED (keys and nonces are refused; seeding is tried again when one is asked for)",
            r.bits,
            r.threshold,
            us / 1000,
            us % 1000
        )
    }
}

/// Seed the global pool: TPM, RDSEED, RDRAND, jitter, mixed, until the
/// threshold is met or the bound of [`SeedPolicy::BOOT`] is reached. Prints
/// the `[entropy]` status line (every image).
pub fn init() {
    let t0 = now_us();
    let mut pool = EntropyPool::new(256);
    let mut plat = Platform::all();
    let report = pool.seed_from(&mut plat, &SeedPolicy::BOOT);
    *POOL.lock() = Some(pool);
    LAST_RETRY_TICK.store(crate::interrupts::ticks(), Ordering::Relaxed);
    crate::serial_println!("{}", status_line(&report, now_us().saturating_sub(t0)));
}

/// Whether the pool has gathered enough real entropy to hand out randomness.
pub fn ready() -> bool {
    POOL.lock().as_ref().map(|p| p.ready()).unwrap_or(false)
}

/// What the seeding gathered so far (`None` before [`init`]).
pub fn report() -> Option<SeedReport> {
    POOL.lock().as_ref().map(|p| p.seed_report())
}

/// Requests refused so far because the pool was not seeded.
pub fn refused() -> u64 {
    REFUSED.load(Ordering::Relaxed)
}

/// One more bounded attempt to seed a pool that is not ready, at most once a
/// second (a request then waits for it; without noise it costs a few
/// milliseconds and changes nothing).
fn seed_again() {
    let now = crate::interrupts::ticks();
    let last = LAST_RETRY_TICK.load(Ordering::Relaxed);
    if now.saturating_sub(last) < 100 && last != 0 {
        return;
    }
    LAST_RETRY_TICK.store(now.max(1), Ordering::Relaxed);
    let mut plat = Platform::all();
    let mut guard = POOL.lock();
    let pool = guard.get_or_insert_with(|| EntropyPool::new(256));
    let was_ready = pool.ready();
    let t0 = now_us();
    let report = pool.seed_from(&mut plat, &SeedPolicy::RETRY);
    drop(guard);
    if report.ready && !was_ready {
        crate::serial_println!("{}", status_line(&report, now_us().saturating_sub(t0)));
    }
}

/// Fill `buf` with CSPRNG output. Returns `false` (and writes nothing) if the
/// pool is not seeded with enough entropy — the hard blocking guarantee. A
/// request that finds the pool closed first waits for one more seeding
/// attempt. The caller must act on `false`: the buffer is as it was.
#[must_use = "false means the CSPRNG is not seeded and the buffer was not written"]
pub fn getrandom(buf: &mut [u8]) -> bool {
    let fill = |buf: &mut [u8]| POOL.lock().as_mut().map(|p| p.fill(buf)).unwrap_or(false);
    if fill(buf) {
        return true;
    }
    seed_again();
    if fill(buf) {
        return true;
    }
    if REFUSED.fetch_add(1, Ordering::Relaxed) == 0 {
        crate::serial_println!("[entropy] a request for random bytes was refused: the CSPRNG is not seeded");
    }
    false
}

/// `N` bytes from the CSPRNG, or `None` when it is not seeded. The form for
/// keys, seeds and nonces: there is no buffer left to use by mistake.
#[must_use]
pub fn draw<const N: usize>() -> Option<[u8; N]> {
    let mut b = [0u8; N];
    if getrandom(&mut b) {
        Some(b)
    } else {
        None
    }
}

/// `[3d8]` boot self-test: prove the entropy gate refuses before it is seeded,
/// that CPU jitter alone reaches the threshold (works with no TPM and no CPU
/// generator), and that the seeded pool yields non-zero, non-repeating output.
pub fn selftest() {
    // (1) A fresh pool refuses to hand out randomness and writes nothing.
    let mut probe = EntropyPool::new(256);
    let empty_refused = {
        let mut b = [0xA7u8; 16];
        !probe.fill(&mut b) && b == [0xA7u8; 16]
    };
    // (2) CPU jitter ALONE can reach readiness (no device dependency), in the
    // 16 batches this test has always allowed.
    let mut jbits = 0usize;
    {
        let mut plat = Platform::jitter_only();
        for _ in 0..16 {
            let s = plat.jitter(512);
            jbits += estimate_jitter_bits(&s);
            probe.add_jitter(&s);
        }
    }
    let jitter_ready = probe.ready();

    // (3) The real global pool (all sources) hands out distinct, non-zero bytes.
    init();
    let from_tpm = crate::tpm::present();
    let is_ready = ready();
    let mixed = report().map(|r| r.jitter_batches >= SeedPolicy::BOOT.min_jitter_batches && r.jitter_bits > 0).unwrap_or(false);
    let a = draw::<32>();
    let b = draw::<32>();
    let distinct = match (a, b) {
        (Some(a), Some(b)) => a != b && a.iter().any(|&x| x != 0),
        _ => false,
    };

    let ok = empty_refused && jitter_ready && is_ready && mixed && distinct;
    crate::serial_println!(
        "[3d8] EuroEntropy CSPRNG (HMAC-DRBG SP800-90A, NIST-KAT-verified): empty-pool-refused={empty_refused}, jitter-alone≈{jbits}bits→ready={jitter_ready}, seeded(jitter+TPM,from-tpm={from_tpm})-ready={is_ready}, jitter-in-the-seed={mixed}, output-nonzero-distinct={distinct} → {}",
        if ok { "OK (getrandom gated on real entropy — no low-entropy output at the worst moment) ✓" } else { "FAILED" }
    );
}
