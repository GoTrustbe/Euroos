//! 3D-8 — the kernel early-boot CSPRNG. Gathers real noise (CPU timing **jitter**
//! + the TPM RNG), seeds an HMAC-DRBG ([`euroentropy`]), and hands out randomness
//! **only** once a security threshold of real entropy is reached. This closes the
//! "weak randomness at the worst moment" gap for FDE, TPM sealing, ML-KEM key
//! generation, the VPN and EuroCA — all of which draw key material very early.

use alloc::vec::Vec;
use euroentropy::{estimate_jitter_bits, EntropyPool};
use spin::Mutex;

static POOL: Mutex<Option<EntropyPool>> = Mutex::new(None);

#[inline]
fn rdtsc() -> u64 {
    // SAFETY: RDTSC is always available on x86-64 and has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Collect `n` CPU-timing-jitter samples: RDTSC deltas around a tiny amount of
/// variable work. The low bits of each delta carry micro-architectural jitter
/// (cache/pipeline/interrupt timing) — a real, device-free noise source.
fn gather_jitter(n: usize) -> Vec<u64> {
    let mut s = Vec::with_capacity(n);
    let mut acc: u64 = 0x9E37_79B9_7F4A_7C15;
    for i in 0..n {
        let t0 = rdtsc();
        acc = acc.wrapping_mul(6364136223846793005).wrapping_add(i as u64);
        core::hint::black_box(acc);
        let t1 = rdtsc();
        s.push(t1.wrapping_sub(t0));
    }
    s
}

/// Seed the global pool from CPU jitter (always) + the TPM RNG (if present).
pub fn init() {
    let mut pool = EntropyPool::new(256);
    // A live TPM RNG is a strong, generously-credited source when present.
    if let Some(b) = crate::tpm::get_random(32) {
        pool.add(&b, 256);
    }
    // Gather CPU jitter until the entropy threshold is actually met (bounded) —
    // this is the "block until enough real entropy" behaviour, and it lets the
    // CSPRNG reach readiness even with no TPM at all.
    for _ in 0..64 {
        if pool.ready() {
            break;
        }
        pool.add_jitter(&gather_jitter(512));
    }
    *POOL.lock() = Some(pool);
}

/// Whether the pool has gathered enough real entropy to hand out randomness.
pub fn ready() -> bool {
    POOL.lock().as_ref().map(|p| p.ready()).unwrap_or(false)
}

/// Fill `buf` with CSPRNG output. Returns `false` (and writes nothing) if the
/// pool is not yet seeded with enough entropy — the hard blocking guarantee.
pub fn getrandom(buf: &mut [u8]) -> bool {
    POOL.lock().as_mut().map(|p| p.fill(buf)).unwrap_or(false)
}

/// `[3d8]` boot self-test: prove the entropy gate refuses before it is seeded,
/// that CPU jitter alone reaches the threshold (works with no TPM), and that the
/// seeded pool yields non-zero, non-repeating output.
pub fn selftest() {
    // (1) A fresh pool refuses to hand out randomness.
    let mut probe = EntropyPool::new(256);
    let empty_refused = {
        let mut b = [0u8; 16];
        !probe.fill(&mut b)
    };
    // (2) CPU jitter ALONE can reach readiness (no device dependency).
    let mut jbits = 0usize;
    for _ in 0..16 {
        let s = gather_jitter(512);
        jbits += estimate_jitter_bits(&s);
        probe.add_jitter(&s);
    }
    let jitter_ready = probe.ready();

    // (3) The real global pool (jitter + TPM) hands out distinct, non-zero bytes.
    init();
    let from_tpm = crate::tpm::present();
    let is_ready = ready();
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    let ga = getrandom(&mut a);
    let gb = getrandom(&mut b);
    let distinct = ga && gb && a != b && a.iter().any(|&x| x != 0);

    let ok = empty_refused && jitter_ready && is_ready && distinct;
    crate::serial_println!(
        "[3d8] EuroEntropy CSPRNG (HMAC-DRBG SP800-90A, NIST-KAT-verified): empty-pool-refused={empty_refused}, jitter-alone≈{jbits}bits→ready={jitter_ready}, seeded(jitter+TPM,from-tpm={from_tpm})-ready={is_ready}, output-nonzero-distinct={distinct} → {}",
        if ok { "OK (getrandom gated on real entropy — no low-entropy output at the worst moment) ✓" } else { "FAILED" }
    );
}
