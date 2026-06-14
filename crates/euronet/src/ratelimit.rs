//! Token-bucket rate limiter for outgoing ICMP error messages
//! (anti-amplification). Without limiting, an attacker with spoofed
//! source IPs could reflect our port-unreachable/RST replies at victims.
//! The bucket is time-based and pure logic → host-testable.

/// A token bucket: at most `capacity` tokens, refilled with `refill_per_sec`
/// tokens per second. Each allowed event costs one token. Time comes
/// from outside (monotonic tick counter) so the logic is deterministically testable.
#[derive(Debug, Clone, Copy)]
pub struct TokenBucket {
    capacity: u32,
    refill_per_sec: u32,
    /// Tokens × `SCALE` (fixed-point, so that sub-token refill is not lost).
    tokens: u64,
    last_ticks: u64,
    ticks_per_sec: u64,
}

const SCALE: u64 = 1_000;

impl TokenBucket {
    /// New bucket, full, with `ticks_per_sec` as the time base (e.g. 100 for the
    /// 100 Hz scheduler tick, or the HPET frequency).
    pub const fn new(capacity: u32, refill_per_sec: u32, ticks_per_sec: u64) -> Self {
        Self {
            capacity,
            refill_per_sec,
            tokens: capacity as u64 * SCALE,
            last_ticks: 0,
            ticks_per_sec: if ticks_per_sec == 0 { 1 } else { ticks_per_sec },
        }
    }

    /// Refill based on the time elapsed since the previous call.
    fn refill(&mut self, now: u64) {
        if now <= self.last_ticks {
            // Time did not advance (or first call): only set the base.
            if self.last_ticks == 0 {
                self.last_ticks = now;
            }
            return;
        }
        let elapsed = now - self.last_ticks;
        // tokens += elapsed/ticks_per_sec * refill_per_sec  (fixed-point × SCALE).
        let add = (elapsed as u128 * self.refill_per_sec as u128 * SCALE as u128 / self.ticks_per_sec as u128) as u64;
        let cap = self.capacity as u64 * SCALE;
        self.tokens = (self.tokens + add).min(cap);
        self.last_ticks = now;
    }

    /// Try to allow one event at time `now` (tick counter).
    /// Returns `true` if a token was available (and consumes it), otherwise
    /// `false` (the event should then be suppressed).
    pub fn allow(&mut self, now: u64) -> bool {
        self.refill(now);
        if self.tokens >= SCALE {
            self.tokens -= SCALE;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_tot_capaciteit_daarna_geblokt() {
        // 5 tokens, 10/s refill, 100 ticks/s.
        let mut tb = TokenBucket::new(5, 10, 100);
        // At t=0 exactly 5 may pass (the full bucket), the 6th may not.
        for _ in 0..5 {
            assert!(tb.allow(0));
        }
        assert!(!tb.allow(0));
    }

    #[test]
    fn bijvulling_over_tijd() {
        let mut tb = TokenBucket::new(5, 10, 100);
        for _ in 0..5 {
            assert!(tb.allow(0));
        }
        assert!(!tb.allow(0));
        // After 10 ticks (0.1 s) at 10/s = 1 token added → exactly one may pass.
        assert!(tb.allow(10));
        assert!(!tb.allow(10));
        // After another 100 ticks (1 s) = 10 tokens, but capped at 5.
        assert!(tb.allow(110));
        let mut allowed = 1;
        while tb.allow(110) {
            allowed += 1;
        }
        assert_eq!(allowed, 5); // capacity, not 10
    }

    #[test]
    fn tijd_achteruit_breekt_niet() {
        let mut tb = TokenBucket::new(2, 1, 100);
        assert!(tb.allow(500));
        // A lower "now" must not refill or panic.
        assert!(tb.allow(400));
        assert!(!tb.allow(400));
    }
}
