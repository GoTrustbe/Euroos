//! EuroWatchdog — a **deadman software watchdog**.
//!
//! A hung kernel is worse than a crashed one: it stops serving without anyone
//! noticing (relevant to the OT/industrial pivot). The watchdog is a liveness
//! deadline: a healthy main loop must **pet** it before the deadline; if a hang
//! stops the petting, the deadline passes and the watchdog **trips**, which the
//! kernel turns into a logged reset. Pure `no_std` timer logic, host-tested; the
//! kernel drives `pet`/`check` from the 100 Hz scheduler tick.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// A liveness deadline measured in ticks.
#[derive(Clone, Copy, Debug)]
pub struct Watchdog {
    /// Ticks of grace after each pet.
    timeout: u64,
    /// Tick by which the next pet must arrive.
    deadline: u64,
    tripped: bool,
    pets: u64,
}

impl Watchdog {
    /// A watchdog that must be petted at least every `timeout` ticks, armed at
    /// `now`.
    pub fn new(timeout: u64, now: u64) -> Watchdog {
        Watchdog { timeout: timeout.max(1), deadline: now.saturating_add(timeout.max(1)), tripped: false, pets: 0 }
    }

    /// The main loop is alive: extend the deadline.
    pub fn pet(&mut self, now: u64) {
        self.deadline = now.saturating_add(self.timeout);
        self.pets += 1;
        self.tripped = false;
    }

    /// Called each tick: returns `true` the moment the deadline is missed
    /// (a hang) — the caller then resets. Latches until petted again.
    pub fn check(&mut self, now: u64) -> bool {
        if now > self.deadline {
            self.tripped = true;
        }
        self.tripped
    }

    pub fn is_tripped(&self) -> bool {
        self.tripped
    }
    pub fn pets(&self) -> u64 {
        self.pets
    }
    /// Ticks remaining before the watchdog would trip (0 if already past).
    pub fn slack(&self, now: u64) -> u64 {
        self.deadline.saturating_sub(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn petting_keeps_it_alive() {
        let mut w = Watchdog::new(100, 0);
        // Pet every 50 ticks → never trips over a long run.
        for t in (0..1000).step_by(50) {
            w.pet(t);
            assert!(!w.check(t + 10));
        }
        assert!(!w.is_tripped());
        assert!(w.pets() >= 19);
    }

    #[test]
    fn a_hang_trips_it() {
        let mut w = Watchdog::new(100, 0);
        w.pet(0);
        assert!(!w.check(50)); // still within grace
        assert!(!w.check(100)); // exactly at the deadline
        assert!(w.check(101)); // deadline missed → trip
        assert!(w.is_tripped());
    }

    #[test]
    fn recovers_after_pet() {
        let mut w = Watchdog::new(100, 0);
        w.pet(0);
        assert!(w.check(200)); // tripped
        w.pet(200); // main loop came back
        assert!(!w.check(250));
    }
}
