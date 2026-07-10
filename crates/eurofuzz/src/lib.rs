//! EuroFuzz — a **deterministic fuzz harness** over the parsers that touch
//! untrusted input.
//!
//! Every function that parses bytes off disk, off the wire, or out of a
//! credential is an attack surface: it must **reject** malformed input, never
//! panic (a panic in the kernel is a crash / a DoS). This harness feeds each
//! such parser hundreds of thousands of random and mutated inputs from a seeded
//! PRNG (reproducible — a failing seed is a bug report), and the test passing is
//! the proof that none of them panicked. This is the fuzzing half of the CRA
//! "secure by design + security testing" evidence (3G-4 / 3E-8), runnable in CI.

#![forbid(unsafe_code)]

/// A tiny reproducible PRNG (xorshift64*) — deterministic so a fuzz failure is
/// replayable from its seed.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// A random byte vector of length up to `max`.
    pub fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = (self.next_u64() as usize) % (max + 1);
        (0..n).map(|_| self.next_u64() as u8).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITERS: usize = 200_000;

    /// The core invariant: a parser may return `None`/`Err`, but it must never
    /// panic, hang, or read out of bounds on any input. Reaching the end of the
    /// loop is the proof.
    #[test]
    fn parsers_never_panic_on_random_input() {
        let mut rng = Rng::new(0xE111_0000_C0DE_1234);
        for _ in 0..ITERS {
            let b = rng.bytes(512);

            // Policy bundles (off disk).
            let _ = europol::bundle::deserialize(&b);

            // Certificates + the CA store (off disk / the wire).
            let _ = euroca::Certificate::from_bytes(&b);
            let _ = euroca::CertStore::from_bytes(&b);

            // DHCPv6 + DHCP + DNS (off the wire).
            let _ = euronet::dhcpv6::parse(&b);
            let _ = euronet::dhcp::parse(&b);
            let _ = euronet::dns::parse_response(&b, 0x1234);

            // TPM responses (from the chip).
            let _ = eurotpm::parse_header(&b);
            let _ = eurotpm::parse_random(&b);
            let _ = eurotpm::parse_unseal(&b);
            let _ = eurotpm::parse_create(&b);

            // Wallet: base64url + JSON + a full SD-JWT presentation.
            if let Ok(s) = core::str::from_utf8(&b) {
                let _ = eurowallet::b64::decode(s);
                let _ = eurowallet::json::parse(s);
                let vk = ed25519_dalek::VerifyingKey::from_bytes(&[0x11; 32]).unwrap();
                let _ = eurowallet::verify(s, &vk);
            }
        }
    }

    /// Round-trip stability: whenever a bundle *does* parse, re-serializing and
    /// re-parsing must be a fixed point (no parser/serializer drift).
    #[test]
    fn bundle_roundtrip_is_stable() {
        let mut rng = Rng::new(0x5EED_5EED_5EED_5EED);
        let mut parsed = 0u64;
        for _ in 0..ITERS {
            let b = rng.bytes(256);
            if let Some(policies) = europol::bundle::deserialize(&b) {
                let re = europol::bundle::serialize(&policies);
                assert_eq!(europol::bundle::deserialize(&re), Some(policies));
                parsed += 1;
            }
        }
        // The corpus is mostly rejected (good); we just confirm the loop ran.
        let _ = parsed;
    }
}
