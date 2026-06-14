//! EuroUpdate — atomic A/B system slots with automatic rollback (plan F1).
//!
//! Two root slots (A/B). An update is written to the INACTIVE slot; the
//! bootloader tries the new slot a bounded number of times (`tries`). If it does
//! not boot successfully (EuroInit then never calls `mark_good`), the next
//! boot automatically rolls back to the last-known-good slot. This way a failed
//! update can never brick the machine — the industry standard (Android/ChromeOS/Fuchsia).
//!
//! Pure `no_std` logic so the error-prone state machine is host-tested.

#![cfg_attr(not(test), no_std)]

/// Which root slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }
    fn to_u8(self) -> u8 {
        match self {
            Slot::A => 0,
            Slot::B => 1,
        }
    }
    fn from_u8(v: u8) -> Slot {
        if v == 1 {
            Slot::B
        } else {
            Slot::A
        }
    }
}

/// State of a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    Empty,  // never held a valid system
    Trying, // just written, not yet confirmed good
    Good,   // confirmed booted successfully
    Failed, // boot attempts exhausted → rejected
}

impl SlotState {
    fn to_u8(self) -> u8 {
        match self {
            SlotState::Empty => 0,
            SlotState::Trying => 1,
            SlotState::Good => 2,
            SlotState::Failed => 3,
        }
    }
    fn from_u8(v: u8) -> SlotState {
        match v {
            1 => SlotState::Trying,
            2 => SlotState::Good,
            3 => SlotState::Failed,
            _ => SlotState::Empty,
        }
    }
}

/// Maximum number of boot attempts for a new slot before rollback.
pub const DEFAULT_TRIES: u8 = 3;

const MAGIC: u32 = 0x4555_5044; // "EUPD"
pub const CONFIG_SIZE: usize = 32;

/// The persistent slot configuration (in `/boot`, read by the bootloader).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotConfig {
    /// The slot the running system came from.
    pub active: Slot,
    /// The slot to be tried on the next boot.
    pub next_boot: Slot,
    /// Remaining boot attempts for `next_boot`.
    pub tries: u8,
    pub state_a: SlotState,
    pub state_b: SlotState,
    /// Generation counter (increments on each update) — for diagnostics/tie-break.
    pub generation: u32,
}

impl Default for SlotConfig {
    fn default() -> Self {
        Self::initial()
    }
}

impl SlotConfig {
    /// Fresh install: slot A holds the (good) system, B is empty.
    pub fn initial() -> Self {
        SlotConfig {
            active: Slot::A,
            next_boot: Slot::A,
            tries: 0,
            state_a: SlotState::Good,
            state_b: SlotState::Empty,
            generation: 1,
        }
    }

    pub fn state(&self, s: Slot) -> SlotState {
        match s {
            Slot::A => self.state_a,
            Slot::B => self.state_b,
        }
    }
    fn set_state(&mut self, s: Slot, st: SlotState) {
        match s {
            Slot::A => self.state_a = st,
            Slot::B => self.state_b = st,
        }
    }

    /// The slot that is NOT active (the target for a new update).
    pub fn inactive(&self) -> Slot {
        self.active.other()
    }

    /// Stage an update: the image has already been written and verified to
    /// `target` (= the inactive slot). Mark it as to-be-tried with fresh attempts.
    pub fn stage_update(&mut self) {
        let target = self.inactive();
        self.set_state(target, SlotState::Trying);
        self.next_boot = target;
        self.tries = DEFAULT_TRIES;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Bootloader logic: determine which slot boots now and update the counter.
    /// Call this once per boot (before loading the kernel).
    pub fn on_boot(&mut self) -> Slot {
        if self.tries > 0 {
            // Still attempts left for the to-be-tried slot.
            self.tries -= 1;
            self.active = self.next_boot;
            self.next_boot
        } else if self.state(self.next_boot) == SlotState::Trying {
            // Attempts exhausted without confirmation → rollback to the good slot.
            self.set_state(self.next_boot, SlotState::Failed);
            let good = self.find_good();
            self.next_boot = good;
            self.active = good;
            good
        } else {
            // Stable: just boot the active/good slot.
            self.active = self.next_boot;
            self.next_boot
        }
    }

    /// EuroInit calls this after a successful boot: the active slot is good.
    pub fn mark_good(&mut self) {
        self.set_state(self.active, SlotState::Good);
        self.tries = 0;
        self.next_boot = self.active;
    }

    /// Force a rollback to the other good slot (manual `euroupdate rollback`).
    pub fn rollback(&mut self) -> bool {
        let other = self.active.other();
        if self.state(other) == SlotState::Good {
            self.next_boot = other;
            self.tries = 0;
            true
        } else {
            false
        }
    }

    /// Find a good slot (preference: the one other than next_boot); fallback slot A.
    fn find_good(&self) -> Slot {
        let cand = self.next_boot.other();
        if self.state(cand) == SlotState::Good {
            cand
        } else if self.state(self.next_boot) == SlotState::Good {
            self.next_boot
        } else {
            Slot::A
        }
    }

    // ── Serialization (fixed 32-byte block with magic + checksum) ──
    pub fn serialize(&self) -> [u8; CONFIG_SIZE] {
        let mut b = [0u8; CONFIG_SIZE];
        b[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        b[4] = 1; // version
        b[5] = self.active.to_u8();
        b[6] = self.next_boot.to_u8();
        b[7] = self.tries;
        b[8] = self.state_a.to_u8();
        b[9] = self.state_b.to_u8();
        b[12..16].copy_from_slice(&self.generation.to_le_bytes());
        let ck = checksum(&b[..CONFIG_SIZE - 4]);
        b[CONFIG_SIZE - 4..].copy_from_slice(&ck.to_le_bytes());
        b
    }

    /// Read the configuration back; `None` on wrong magic or checksum (in which case
    /// the caller should fall back to [`SlotConfig::initial`]).
    pub fn deserialize(b: &[u8]) -> Option<SlotConfig> {
        if b.len() < CONFIG_SIZE {
            return None;
        }
        if u32::from_le_bytes(b[0..4].try_into().ok()?) != MAGIC {
            return None;
        }
        let stored = u32::from_le_bytes(b[CONFIG_SIZE - 4..CONFIG_SIZE].try_into().ok()?);
        if checksum(&b[..CONFIG_SIZE - 4]) != stored {
            return None;
        }
        Some(SlotConfig {
            active: Slot::from_u8(b[5]),
            next_boot: Slot::from_u8(b[6]),
            tries: b[7],
            state_a: SlotState::from_u8(b[8]),
            state_b: SlotState::from_u8(b[9]),
            generation: u32::from_le_bytes(b[12..16].try_into().ok()?),
        })
    }
}

/// Simple Fletcher-32-like checksum over the config block.
fn checksum(data: &[u8]) -> u32 {
    let mut s1: u32 = 0xFFFF;
    let mut s2: u32 = 0xFFFF;
    for &b in data {
        s1 = (s1 + b as u32) % 65535;
        s2 = (s2 + s1) % 65535;
    }
    (s2 << 16) | s1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_update_marks_good() {
        let mut c = SlotConfig::initial();
        assert_eq!(c.active, Slot::A);
        assert_eq!(c.inactive(), Slot::B);
        // Update to B (image already written out).
        c.stage_update();
        assert_eq!(c.next_boot, Slot::B);
        assert_eq!(c.tries, DEFAULT_TRIES);
        assert_eq!(c.state(Slot::B), SlotState::Trying);
        // Reboot: bootloader tries B (tries 3→2), B boots OK → mark_good.
        assert_eq!(c.on_boot(), Slot::B);
        assert_eq!(c.tries, 2);
        c.mark_good();
        assert_eq!(c.state(Slot::B), SlotState::Good);
        assert_eq!(c.active, Slot::B);
        assert_eq!(c.tries, 0);
        // Next boot stays stable on B.
        assert_eq!(c.on_boot(), Slot::B);
    }

    #[test]
    fn failed_update_rolls_back_after_tries() {
        let mut c = SlotConfig::initial(); // A good, B empty
        c.stage_update(); // try B, tries=3
        // B keeps crashing (never mark_good): three attempts, then rollback.
        assert_eq!(c.on_boot(), Slot::B); // tries 3→2
        assert_eq!(c.on_boot(), Slot::B); // 2→1
        assert_eq!(c.on_boot(), Slot::B); // 1→0
        // Fourth boot: attempts gone, B was Trying → rollback to A.
        assert_eq!(c.on_boot(), Slot::A);
        assert_eq!(c.state(Slot::B), SlotState::Failed);
        assert_eq!(c.active, Slot::A);
        assert_eq!(c.state(Slot::A), SlotState::Good);
    }

    #[test]
    fn manual_rollback_when_other_good() {
        let mut c = SlotConfig::initial();
        c.stage_update();
        c.on_boot();
        c.mark_good(); // now B runs, A still good
        assert!(c.rollback()); // back to A
        assert_eq!(c.next_boot, Slot::A);
        // But if the other slot is not good, rollback fails.
        let mut fresh = SlotConfig::initial(); // B is Empty
        assert!(!fresh.rollback());
    }

    #[test]
    fn serialize_roundtrip_and_reject_corruption() {
        let mut c = SlotConfig::initial();
        c.stage_update();
        c.on_boot();
        let bytes = c.serialize();
        assert_eq!(SlotConfig::deserialize(&bytes), Some(c));
        // Flip one bit → checksum fails → None (fall back to initial).
        let mut bad = bytes;
        bad[7] ^= 0xFF;
        assert_eq!(SlotConfig::deserialize(&bad), None);
        // Wrong magic → None.
        let mut nomagic = bytes;
        nomagic[0] ^= 0xFF;
        assert_eq!(SlotConfig::deserialize(&nomagic), None);
        // Too short → None.
        assert_eq!(SlotConfig::deserialize(&bytes[..10]), None);
    }

    #[test]
    fn alternating_updates_use_inactive_slot() {
        let mut c = SlotConfig::initial(); // active A
        c.stage_update(); // → B
        c.on_boot();
        c.mark_good(); // active B
        assert_eq!(c.inactive(), Slot::A);
        c.stage_update(); // next update goes to A
        assert_eq!(c.next_boot, Slot::A);
        c.on_boot();
        c.mark_good();
        assert_eq!(c.active, Slot::A);
        assert_eq!(c.generation, 3); // initial=1, +2 updates
    }
}
