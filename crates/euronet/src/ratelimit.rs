//! Token-bucket snelheidsbegrenzer voor uitgaande ICMP-foutmeldingen
//! (anti-amplificatie). Zonder begrenzing kan een aanvaller met vervalste
//! bron-IP's onze poort-onbereikbaar/RST-antwoorden naar slachtoffers laten
//! reflecteren. De bucket is tijdsgebaseerd en pure logica → host-testbaar.

/// Een token-bucket: maximaal `capacity` tokens, bijgevuld met `refill_per_sec`
/// tokens per seconde. Elke toegelaten gebeurtenis kost één token. De tijd komt
/// van buitenaf (monotone tick-teller) zodat de logica deterministisch testbaar is.
#[derive(Debug, Clone, Copy)]
pub struct TokenBucket {
    capacity: u32,
    refill_per_sec: u32,
    /// Tokens × `SCALE` (vaste-komma, zodat sub-token-bijvulling niet verloren gaat).
    tokens: u64,
    last_ticks: u64,
    ticks_per_sec: u64,
}

const SCALE: u64 = 1_000;

impl TokenBucket {
    /// Nieuwe bucket, vol, met `ticks_per_sec` als tijdbasis (bv. 100 voor de
    /// 100 Hz scheduler-tick, of de HPET-frequentie).
    pub const fn new(capacity: u32, refill_per_sec: u32, ticks_per_sec: u64) -> Self {
        Self {
            capacity,
            refill_per_sec,
            tokens: capacity as u64 * SCALE,
            last_ticks: 0,
            ticks_per_sec: if ticks_per_sec == 0 { 1 } else { ticks_per_sec },
        }
    }

    /// Vul bij op basis van de verstreken tijd sinds de vorige aanroep.
    fn refill(&mut self, now: u64) {
        if now <= self.last_ticks {
            // Tijd niet vooruit (of eerste call): alleen de basis zetten.
            if self.last_ticks == 0 {
                self.last_ticks = now;
            }
            return;
        }
        let elapsed = now - self.last_ticks;
        // tokens += elapsed/ticks_per_sec * refill_per_sec  (vaste-komma × SCALE).
        let add = (elapsed as u128 * self.refill_per_sec as u128 * SCALE as u128 / self.ticks_per_sec as u128) as u64;
        let cap = self.capacity as u64 * SCALE;
        self.tokens = (self.tokens + add).min(cap);
        self.last_ticks = now;
    }

    /// Probeer één gebeurtenis toe te laten op tijdstip `now` (tick-teller).
    /// Geeft `true` als er een token beschikbaar was (en verbruikt het), anders
    /// `false` (de gebeurtenis hoort dan onderdrukt te worden).
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
        // 5 tokens, 10/s bijvulling, 100 ticks/s.
        let mut tb = TokenBucket::new(5, 10, 100);
        // Op t=0 mogen er precies 5 door (de volle bucket), de 6e niet.
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
        // Na 10 ticks (0,1 s) bij 10/s = 1 token bij → precies één mag door.
        assert!(tb.allow(10));
        assert!(!tb.allow(10));
        // Na nóg 100 ticks (1 s) = 10 tokens, maar gecapt op 5.
        assert!(tb.allow(110));
        let mut allowed = 1;
        while tb.allow(110) {
            allowed += 1;
        }
        assert_eq!(allowed, 5); // capaciteit, niet 10
    }

    #[test]
    fn tijd_achteruit_breekt_niet() {
        let mut tb = TokenBucket::new(2, 1, 100);
        assert!(tb.allow(500));
        // Een lagere "now" mag niet bijvullen of paniekeren.
        assert!(tb.allow(400));
        assert!(!tb.allow(400));
    }
}
