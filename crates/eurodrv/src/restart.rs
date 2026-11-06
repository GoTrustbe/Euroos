//! Restart policy for a crashed driver.
//!
//! A driver that dies is restarted after a backoff that doubles with every
//! death inside the window; once `max_restarts` deaths fall inside one window
//! the supervisor gives up (a crash loop must not eat the machine) and the
//! device stays quiesced until someone restarts it by hand. Time is in
//! scheduler ticks (10 ms).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Deaths tolerated inside one window before giving up.
    pub max_restarts: u32,
    /// Length of the window, in ticks.
    pub window_ticks: u64,
    /// Delay before the first restart; doubles per further death in the window.
    pub backoff_ticks: u64,
    /// Upper bound for the doubled delay.
    pub max_backoff_ticks: u64,
}

impl Policy {
    /// The default for NIC drivers: 5 deaths per minute, restart at once the
    /// first time, then 20 ms, 40 ms, ... capped at 2 s.
    pub const NIC: Policy = Policy { max_restarts: 5, window_ticks: 6000, backoff_ticks: 2, max_backoff_ticks: 200 };
    /// Disk drivers: 3 deaths per minute, then the disk stays off. A storage
    /// driver in a crash loop can corrupt what it was writing over and over,
    /// so it gets fewer chances and a longer backoff than a NIC: the first
    /// restart at once, then 100 ms, 200 ms, ..., capped at 5 s.
    pub const DISK: Policy = Policy { max_restarts: 3, window_ticks: 6000, backoff_ticks: 10, max_backoff_ticks: 500 };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Start a new instance at this tick.
    RestartAt(u64),
    /// Too many deaths in the window: leave the device off.
    GiveUp,
}

/// Remembers the recent deaths of one driver.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tracker {
    deaths: [u64; 16],
    n: usize,
    total: u64,
}

impl Tracker {
    pub const fn new() -> Tracker {
        Tracker { deaths: [0; 16], n: 0, total: 0 }
    }

    /// Deaths recorded since the tracker was created.
    pub fn total_deaths(&self) -> u64 {
        self.total
    }

    /// Record a death at `now` and decide.
    pub fn on_death(&mut self, now: u64, p: &Policy) -> Decision {
        self.total += 1;
        // Keep only deaths inside the window ending now.
        let since = now.saturating_sub(p.window_ticks);
        let mut keep = [0u64; 16];
        let mut k = 0;
        for &t in &self.deaths[..self.n] {
            if t >= since && k < keep.len() - 1 {
                keep[k] = t;
                k += 1;
            }
        }
        keep[k] = now;
        k += 1;
        self.deaths = keep;
        self.n = k;
        if k as u32 > p.max_restarts {
            return Decision::GiveUp;
        }
        // First death in the window: no delay beyond backoff_ticks * 0 ... the
        // first restart is immediate, the next ones back off.
        let exp = (k - 1).min(32) as u32;
        let delay = if exp == 0 {
            0
        } else {
            p.backoff_ticks.saturating_mul(1u64 << (exp - 1)).min(p.max_backoff_ticks)
        };
        Decision::RestartAt(now.saturating_add(delay))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disk_driver_gets_fewer_chances_than_a_nic() {
        let p = Policy::DISK;
        let mut t = Tracker::new();
        assert_eq!(t.on_death(1000, &p), Decision::RestartAt(1000));
        assert_eq!(t.on_death(1100, &p), Decision::RestartAt(1110));
        assert_eq!(t.on_death(1200, &p), Decision::RestartAt(1220));
        assert_eq!(t.on_death(1300, &p), Decision::GiveUp);
        assert!(Policy::DISK.max_restarts < Policy::NIC.max_restarts);
    }

    #[test]
    fn first_death_restarts_immediately_then_backs_off() {
        let p = Policy::NIC;
        let mut t = Tracker::new();
        assert_eq!(t.on_death(1000, &p), Decision::RestartAt(1000));
        assert_eq!(t.on_death(1010, &p), Decision::RestartAt(1012));
        assert_eq!(t.on_death(1020, &p), Decision::RestartAt(1024));
        assert_eq!(t.on_death(1030, &p), Decision::RestartAt(1038));
        assert_eq!(t.on_death(1040, &p), Decision::RestartAt(1056));
        assert_eq!(t.total_deaths(), 5);
    }

    #[test]
    fn a_crash_loop_gives_up() {
        let p = Policy::NIC;
        let mut t = Tracker::new();
        for i in 0..5 {
            assert!(matches!(t.on_death(i * 10, &p), Decision::RestartAt(_)));
        }
        assert_eq!(t.on_death(60, &p), Decision::GiveUp);
        assert_eq!(t.on_death(70, &p), Decision::GiveUp, "stays given up inside the window");
    }

    #[test]
    fn old_deaths_age_out_of_the_window() {
        let p = Policy { max_restarts: 2, window_ticks: 100, backoff_ticks: 4, max_backoff_ticks: 1000 };
        let mut t = Tracker::new();
        assert_eq!(t.on_death(0, &p), Decision::RestartAt(0));
        assert_eq!(t.on_death(50, &p), Decision::RestartAt(54));
        // 0 and 50 are older than 100 ticks at 300: a fresh window.
        assert_eq!(t.on_death(300, &p), Decision::RestartAt(300));
        assert_eq!(t.on_death(310, &p), Decision::RestartAt(314));
        assert_eq!(t.on_death(320, &p), Decision::GiveUp);
    }

    #[test]
    fn backoff_is_capped_and_never_overflows() {
        let p = Policy { max_restarts: 15, window_ticks: u64::MAX, backoff_ticks: u64::MAX / 2, max_backoff_ticks: 500 };
        let mut t = Tracker::new();
        let mut last = Decision::GiveUp;
        for i in 0..15u64 {
            last = t.on_death(i, &p);
        }
        assert_eq!(last, Decision::RestartAt(14 + 500));
        assert_eq!(t.on_death(u64::MAX, &p), Decision::GiveUp);
    }
}
