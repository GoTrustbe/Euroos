//! TCP timing and congestion management: an RTT/RTO estimator (RFC 6298) and a Reno
//! congestion controller (RFC 5681). Pure, deterministic logic — tested apart from
//! the kernel socket, so the error-prone arithmetic is host-testable.

/// RTT/RTO estimator per RFC 6298. All times in milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct RttEstimator {
    srtt: i64,   // smoothed RTT (ms), -1 = no measurement yet
    rttvar: i64, // RTT variance (ms)
    rto: i64,    // current retransmission timeout (ms)
}

const MIN_RTO: i64 = 1_000; // RFC 6298: lower bound 1 s
const MAX_RTO: i64 = 60_000; // practical upper bound 60 s
const INIT_RTO: i64 = 1_000; // initial value before the first measurement

impl Default for RttEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl RttEstimator {
    pub const fn new() -> Self {
        Self { srtt: -1, rttvar: 0, rto: INIT_RTO }
    }

    /// Current retransmission timeout in ms.
    pub fn rto_ms(&self) -> i64 {
        self.rto.clamp(MIN_RTO, MAX_RTO)
    }

    /// Process an RTT measurement (ms). Karn's algorithm: the caller must NOT
    /// pass a measurement of a retransmitted segment (ambiguous RTT).
    pub fn on_measurement(&mut self, r: i64) {
        let r = r.max(1);
        if self.srtt < 0 {
            // First measurement (RFC 6298 §2.2).
            self.srtt = r;
            self.rttvar = r / 2;
        } else {
            // Subsequent measurement (§2.3): RTTVAR and SRTT with β=1/4, α=1/8.
            let delta = self.srtt - r;
            self.rttvar = self.rttvar + ((delta.abs() - self.rttvar) >> 2);
            self.srtt = self.srtt + ((r - self.srtt) >> 3);
        }
        // RTO = SRTT + max(G, K·RTTVAR), K=4, G≈1 ms.
        self.rto = (self.srtt + (4 * self.rttvar).max(1)).clamp(MIN_RTO, MAX_RTO);
    }

    /// A timeout occurred: exponential backoff (RFC 6298 §5.5), capped.
    pub fn on_timeout(&mut self) {
        self.rto = (self.rto * 2).min(MAX_RTO);
    }
}

/// Reno congestion controller (RFC 5681). Windows in bytes; `mss` = max segment size.
#[derive(Debug, Clone, Copy)]
pub struct RenoCc {
    mss: u32,
    cwnd: u32,
    ssthresh: u32,
}

impl RenoCc {
    /// New controller: cwnd = 1·MSS (conservative start), ssthresh "infinite".
    pub fn new(mss: u32) -> Self {
        let mss = mss.max(1);
        Self { mss, cwnd: mss, ssthresh: u32::MAX }
    }

    pub fn cwnd(&self) -> u32 {
        self.cwnd
    }
    pub fn ssthresh(&self) -> u32 {
        self.ssthresh
    }
    /// In slow-start as long as cwnd < ssthresh.
    pub fn in_slow_start(&self) -> bool {
        self.cwnd < self.ssthresh
    }

    /// A newly-acknowledging ACK. In slow-start cwnd grows by 1·MSS per ACK
    /// (exponential/RTT); in congestion avoidance by ≈MSS²/cwnd per ACK
    /// (linear/RTT).
    pub fn on_ack(&mut self) {
        if self.in_slow_start() {
            self.cwnd = self.cwnd.saturating_add(self.mss);
        } else {
            let inc = (self.mss as u64 * self.mss as u64 / self.cwnd.max(1) as u64).max(1) as u32;
            self.cwnd = self.cwnd.saturating_add(inc);
        }
    }

    /// Retransmission timeout: ssthresh = max(flight/2, 2·MSS), cwnd = 1·MSS
    /// (back to slow-start). `flight` = bytes in flight at the timeout.
    pub fn on_timeout(&mut self, flight: u32) {
        self.ssthresh = (flight / 2).max(2 * self.mss);
        self.cwnd = self.mss;
    }

    /// Three duplicate ACKs → fast retransmit + fast recovery (RFC 5681 §3.2):
    /// ssthresh = max(flight/2, 2·MSS), cwnd = ssthresh (no full reset).
    pub fn on_triple_dup_ack(&mut self, flight: u32) {
        self.ssthresh = (flight / 2).max(2 * self.mss);
        self.cwnd = self.ssthresh;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rto_eerste_meting() {
        let mut e = RttEstimator::new();
        assert_eq!(e.rto_ms(), 1000); // init
        e.on_measurement(200);
        // SRTT=200, RTTVAR=100, RTO=200+400=600 → clamped to 1000 (lower bound).
        assert_eq!(e.rto_ms(), 1000);
    }

    #[test]
    fn rto_convergeert_naar_stabiele_rtt() {
        let mut e = RttEstimator::new();
        // A high RTT so that the lower bound does not mask everything.
        for _ in 0..50 {
            e.on_measurement(500);
        }
        // SRTT → 500, RTTVAR → ~0, RTO → ~500 but ≥ 1000 (lower bound).
        assert!(e.rto_ms() >= 1000);
        // With variation RTTVAR (and thus RTO) rises.
        let stable = e.rto_ms();
        for i in 0..20 {
            e.on_measurement(if i % 2 == 0 { 200 } else { 800 });
        }
        assert!(e.rto_ms() >= stable);
    }

    #[test]
    fn rto_backoff_en_cap() {
        let mut e = RttEstimator::new();
        e.on_measurement(20_000); // high RTT → large RTO
        let before = e.rto_ms();
        e.on_timeout();
        assert!(e.rto_ms() >= before); // doubling (capped at 60 s)
        for _ in 0..10 {
            e.on_timeout();
        }
        assert_eq!(e.rto_ms(), 60_000); // capped
    }

    #[test]
    fn reno_slow_start_dan_avoidance() {
        let mss = 1460;
        let mut cc = RenoCc::new(mss);
        assert_eq!(cc.cwnd(), mss);
        assert!(cc.in_slow_start());
        // Force an ssthresh via a timeout, then observe growth.
        cc.on_timeout(40 * mss); // ssthresh = 20·MSS, cwnd = 1·MSS
        assert_eq!(cc.ssthresh(), 20 * mss);
        assert_eq!(cc.cwnd(), mss);
        // Slow-start: per ACK +MSS up to ssthresh.
        let mut acks = 0;
        while cc.in_slow_start() {
            cc.on_ack();
            acks += 1;
        }
        assert!(acks <= 20 && cc.cwnd() >= cc.ssthresh());
        // Then avoidance: growth per ACK is much smaller than an MSS.
        let before = cc.cwnd();
        cc.on_ack();
        assert!(cc.cwnd() - before < mss);
    }

    #[test]
    fn reno_fast_recovery_halveert() {
        let mss = 1000;
        let mut cc = RenoCc::new(mss);
        // Bring cwnd up.
        cc.on_timeout(100 * mss); // ssthresh 50·MSS
        for _ in 0..60 {
            cc.on_ack();
        }
        let flight = cc.cwnd();
        cc.on_triple_dup_ack(flight);
        // Fast recovery: cwnd = ssthresh = flight/2 (no full reset to 1 MSS).
        assert_eq!(cc.ssthresh(), (flight / 2).max(2 * mss));
        assert_eq!(cc.cwnd(), cc.ssthresh());
        assert!(cc.cwnd() > mss);
    }
}
