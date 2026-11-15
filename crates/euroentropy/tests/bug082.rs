//! BUG-082, through the public interface only (these also compile against the
//! crate before the health tests existed, and fail there): a noise source that
//! is broken in one of the two ways SP 800-90B's health tests catch must not
//! seed the pool.

use euroentropy::{EntropyPool, NoiseSources, SeedPolicy};

struct Src {
    x: u64,
    periodic: bool,
    stuck_rdrand: bool,
}

impl Src {
    fn next(&mut self) -> u64 {
        // xorshift64: test data, not randomness.
        self.x ^= self.x << 13;
        self.x ^= self.x >> 7;
        self.x ^= self.x << 17;
        self.x
    }
}

impl NoiseSources for Src {
    fn rdrand(&mut self) -> Option<u64> {
        Some(if self.stuck_rdrand { u64::MAX } else { self.next() })
    }
    fn jitter(&mut self, n: usize) -> Vec<u64> {
        if self.periodic {
            // The two-instruction sample of the BUG-065 diagnosis.
            let pat = [12u64, 12, 12, 12, 38, 12, 12, 12, 14, 38];
            (0..n).map(|i| pat[i % pat.len()]).collect()
        } else {
            (0..n).map(|_| 88 + self.next() % 140).collect()
        }
    }
}

const P: SeedPolicy = SeedPolicy { jitter_batch: 512, min_jitter_batches: 4, max_jitter_batches: 64, cpu_words: 8 };

#[test]
fn a_periodic_jitter_series_does_not_open_the_pool() {
    let mut src = Src { x: 0x1234_5678_9abc_def1, periodic: true, stuck_rdrand: false };
    let mut p = EntropyPool::new(256);
    let r = p.seed_from(&mut src, &P);
    assert!(!r.ready, "a clock beat opened the pool: {} jitter bits credited", r.jitter_bits);
}

#[test]
fn the_words_of_a_stuck_rdrand_are_not_used() {
    let mut src = Src { x: 0x1234_5678_9abc_def1, periodic: false, stuck_rdrand: true };
    let mut p = EntropyPool::new(256);
    let r = p.seed_from(&mut src, &P);
    assert_eq!(r.rdrand_words, 0, "all-ones words from RDRAND were mixed in and credited ({} CPU bits)", r.cpu_bits);
    assert!(r.ready, "jitter alone still seeds the pool");
}
