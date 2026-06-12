//! TCP-tijd- en congestiebeheer: een RTT/RTO-schatter (RFC 6298) en een Reno-
//! congestieregelaar (RFC 5681). Pure, deterministische logica — los van de
//! kernel-socket getest, zodat de fout-gevoelige rekenkunde host-testbaar is.

/// RTT/RTO-schatter volgens RFC 6298. Alle tijden in milliseconden.
#[derive(Debug, Clone, Copy)]
pub struct RttEstimator {
    srtt: i64,   // smoothed RTT (ms), -1 = nog geen meting
    rttvar: i64, // RTT-variantie (ms)
    rto: i64,    // huidige retransmission timeout (ms)
}

const MIN_RTO: i64 = 1_000; // RFC 6298: ondergrens 1 s
const MAX_RTO: i64 = 60_000; // praktische bovengrens 60 s
const INIT_RTO: i64 = 1_000; // beginwaarde vóór de eerste meting

impl Default for RttEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl RttEstimator {
    pub const fn new() -> Self {
        Self { srtt: -1, rttvar: 0, rto: INIT_RTO }
    }

    /// Huidige retransmission timeout in ms.
    pub fn rto_ms(&self) -> i64 {
        self.rto.clamp(MIN_RTO, MAX_RTO)
    }

    /// Verwerk een RTT-meting (ms). Karn's algoritme: de aanroeper mag GEEN
    /// meting van een geretransmitteerd segment doorgeven (dubbelzinnige RTT).
    pub fn on_measurement(&mut self, r: i64) {
        let r = r.max(1);
        if self.srtt < 0 {
            // Eerste meting (RFC 6298 §2.2).
            self.srtt = r;
            self.rttvar = r / 2;
        } else {
            // Vervolgmeting (§2.3): RTTVAR en SRTT met β=1/4, α=1/8.
            let delta = self.srtt - r;
            self.rttvar = self.rttvar + ((delta.abs() - self.rttvar) >> 2);
            self.srtt = self.srtt + ((r - self.srtt) >> 3);
        }
        // RTO = SRTT + max(G, K·RTTVAR), K=4, G≈1 ms.
        self.rto = (self.srtt + (4 * self.rttvar).max(1)).clamp(MIN_RTO, MAX_RTO);
    }

    /// Een timeout trad op: exponentiële backoff (RFC 6298 §5.5), gecapt.
    pub fn on_timeout(&mut self) {
        self.rto = (self.rto * 2).min(MAX_RTO);
    }
}

/// Reno-congestieregelaar (RFC 5681). Vensters in bytes; `mss` = max segmentgrootte.
#[derive(Debug, Clone, Copy)]
pub struct RenoCc {
    mss: u32,
    cwnd: u32,
    ssthresh: u32,
}

impl RenoCc {
    /// Nieuwe regelaar: cwnd = 1·MSS (conservatieve start), ssthresh "oneindig".
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
    /// In slow-start zolang cwnd < ssthresh.
    pub fn in_slow_start(&self) -> bool {
        self.cwnd < self.ssthresh
    }

    /// Een nieuw-bevestigend ACK. In slow-start groeit cwnd met 1·MSS per ACK
    /// (exponentieel/RTT); in congestion avoidance met ≈MSS²/cwnd per ACK
    /// (lineair/RTT).
    pub fn on_ack(&mut self) {
        if self.in_slow_start() {
            self.cwnd = self.cwnd.saturating_add(self.mss);
        } else {
            let inc = (self.mss as u64 * self.mss as u64 / self.cwnd.max(1) as u64).max(1) as u32;
            self.cwnd = self.cwnd.saturating_add(inc);
        }
    }

    /// Retransmission-timeout: ssthresh = max(flight/2, 2·MSS), cwnd = 1·MSS
    /// (terug naar slow-start). `flight` = bytes in flight bij de timeout.
    pub fn on_timeout(&mut self, flight: u32) {
        self.ssthresh = (flight / 2).max(2 * self.mss);
        self.cwnd = self.mss;
    }

    /// Drie dubbele ACK's → fast retransmit + fast recovery (RFC 5681 §3.2):
    /// ssthresh = max(flight/2, 2·MSS), cwnd = ssthresh (geen volledige reset).
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
        // SRTT=200, RTTVAR=100, RTO=200+400=600 → geklemd op 1000 (ondergrens).
        assert_eq!(e.rto_ms(), 1000);
    }

    #[test]
    fn rto_convergeert_naar_stabiele_rtt() {
        let mut e = RttEstimator::new();
        // Een hoge RTT zodat de ondergrens niet alles maskeert.
        for _ in 0..50 {
            e.on_measurement(500);
        }
        // SRTT → 500, RTTVAR → ~0, RTO → ~500 maar ≥ 1000 (ondergrens).
        assert!(e.rto_ms() >= 1000);
        // Met variatie loopt RTTVAR (en dus RTO) op.
        let stable = e.rto_ms();
        for i in 0..20 {
            e.on_measurement(if i % 2 == 0 { 200 } else { 800 });
        }
        assert!(e.rto_ms() >= stable);
    }

    #[test]
    fn rto_backoff_en_cap() {
        let mut e = RttEstimator::new();
        e.on_measurement(20_000); // hoge RTT → RTO groot
        let before = e.rto_ms();
        e.on_timeout();
        assert!(e.rto_ms() >= before); // verdubbeling (gecapt op 60 s)
        for _ in 0..10 {
            e.on_timeout();
        }
        assert_eq!(e.rto_ms(), 60_000); // gecapt
    }

    #[test]
    fn reno_slow_start_dan_avoidance() {
        let mss = 1460;
        let mut cc = RenoCc::new(mss);
        assert_eq!(cc.cwnd(), mss);
        assert!(cc.in_slow_start());
        // Forceer een ssthresh via een timeout, dan groei observeren.
        cc.on_timeout(40 * mss); // ssthresh = 20·MSS, cwnd = 1·MSS
        assert_eq!(cc.ssthresh(), 20 * mss);
        assert_eq!(cc.cwnd(), mss);
        // Slow-start: per ACK +MSS tot ssthresh.
        let mut acks = 0;
        while cc.in_slow_start() {
            cc.on_ack();
            acks += 1;
        }
        assert!(acks <= 20 && cc.cwnd() >= cc.ssthresh());
        // Daarna avoidance: groei per ACK is veel kleiner dan een MSS.
        let before = cc.cwnd();
        cc.on_ack();
        assert!(cc.cwnd() - before < mss);
    }

    #[test]
    fn reno_fast_recovery_halveert() {
        let mss = 1000;
        let mut cc = RenoCc::new(mss);
        // Breng cwnd omhoog.
        cc.on_timeout(100 * mss); // ssthresh 50·MSS
        for _ in 0..60 {
            cc.on_ack();
        }
        let flight = cc.cwnd();
        cc.on_triple_dup_ack(flight);
        // Fast recovery: cwnd = ssthresh = flight/2 (geen volledige reset naar 1 MSS).
        assert_eq!(cc.ssthresh(), (flight / 2).max(2 * mss));
        assert_eq!(cc.cwnd(), cc.ssthresh());
        assert!(cc.cwnd() > mss);
    }
}
