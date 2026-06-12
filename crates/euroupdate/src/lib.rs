//! EuroUpdate — atomische A/B-systeemslots met automatische rollback (plan F1).
//!
//! Twee root-slots (A/B). Een update wordt naar het INACTIEVE slot geschreven; de
//! bootloader probeert het nieuwe slot een begrensd aantal keer (`tries`). Boot het
//! niet succesvol (EuroInit roept dan `mark_good` niet aan), dan rolt de volgende
//! boot automatisch terug naar het laatst-bekende-goede slot. Zo kan een mislukte
//! update de machine nooit bricken — de industriestandaard (Android/ChromeOS/Fuchsia).
//!
//! Pure `no_std`-logica zodat de fout-gevoelige toestandsmachine host-getest is.

#![cfg_attr(not(test), no_std)]

/// Welk root-slot.
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

/// Toestand van een slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    Empty,  // nog nooit een geldig systeem
    Trying, // net beschreven, nog niet bevestigd goed
    Good,   // bevestigd succesvol geboot
    Failed, // boot-pogingen uitgeput → afgekeurd
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

/// Maximaal aantal boot-pogingen voor een nieuw slot vóór rollback.
pub const DEFAULT_TRIES: u8 = 3;

const MAGIC: u32 = 0x4555_5044; // "EUPD"
pub const CONFIG_SIZE: usize = 32;

/// De persistente slot-configuratie (in `/boot`, door de bootloader gelezen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotConfig {
    /// Het slot waar het draaiende systeem vandaan kwam.
    pub active: Slot,
    /// Het slot dat de volgende boot geprobeerd moet worden.
    pub next_boot: Slot,
    /// Resterende boot-pogingen voor `next_boot`.
    pub tries: u8,
    pub state_a: SlotState,
    pub state_b: SlotState,
    /// Generatieteller (loopt op bij elke update) — voor diagnose/tie-break.
    pub generation: u32,
}

impl Default for SlotConfig {
    fn default() -> Self {
        Self::initial()
    }
}

impl SlotConfig {
    /// Verse installatie: slot A bevat het (goede) systeem, B is leeg.
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

    /// Het slot dat NIET actief is (het doel voor een nieuwe update).
    pub fn inactive(&self) -> Slot {
        self.active.other()
    }

    /// Stage een update: het image is al naar `target` (= het inactieve slot)
    /// geschreven en geverifieerd. Markeer het als te-proberen met verse pogingen.
    pub fn stage_update(&mut self) {
        let target = self.inactive();
        self.set_state(target, SlotState::Trying);
        self.next_boot = target;
        self.tries = DEFAULT_TRIES;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Bootloader-logica: bepaal welk slot nu geboot wordt en werk de teller bij.
    /// Roep dit één keer per boot aan (vóór het laden van de kernel).
    pub fn on_boot(&mut self) -> Slot {
        if self.tries > 0 {
            // Nog pogingen over voor het te-proberen slot.
            self.tries -= 1;
            self.active = self.next_boot;
            self.next_boot
        } else if self.state(self.next_boot) == SlotState::Trying {
            // Pogingen uitgeput zonder bevestiging → rollback naar het goede slot.
            self.set_state(self.next_boot, SlotState::Failed);
            let good = self.find_good();
            self.next_boot = good;
            self.active = good;
            good
        } else {
            // Stabiel: boot gewoon het actieve/goede slot.
            self.active = self.next_boot;
            self.next_boot
        }
    }

    /// EuroInit roept dit aan ná een succesvolle boot: het actieve slot is goed.
    pub fn mark_good(&mut self) {
        self.set_state(self.active, SlotState::Good);
        self.tries = 0;
        self.next_boot = self.active;
    }

    /// Forceer een rollback naar het andere goede slot (handmatig `euroupdate rollback`).
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

    /// Zoek een goed slot (voorkeur: het andere dan next_boot); fallback slot A.
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

    // ── Serialisatie (vast 32-byte blok met magic + checksum) ──
    pub fn serialize(&self) -> [u8; CONFIG_SIZE] {
        let mut b = [0u8; CONFIG_SIZE];
        b[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        b[4] = 1; // versie
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

    /// Lees de configuratie terug; `None` bij verkeerde magic of checksum (dan
    /// hoort de aanroeper terug te vallen op [`SlotConfig::initial`]).
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

/// Eenvoudige Fletcher-32-achtige checksum over het configblok.
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
        // Update naar B (image al weggeschreven).
        c.stage_update();
        assert_eq!(c.next_boot, Slot::B);
        assert_eq!(c.tries, DEFAULT_TRIES);
        assert_eq!(c.state(Slot::B), SlotState::Trying);
        // Reboot: bootloader probeert B (tries 3→2), B boot OK → mark_good.
        assert_eq!(c.on_boot(), Slot::B);
        assert_eq!(c.tries, 2);
        c.mark_good();
        assert_eq!(c.state(Slot::B), SlotState::Good);
        assert_eq!(c.active, Slot::B);
        assert_eq!(c.tries, 0);
        // Volgende boot blijft stabiel op B.
        assert_eq!(c.on_boot(), Slot::B);
    }

    #[test]
    fn failed_update_rolls_back_after_tries() {
        let mut c = SlotConfig::initial(); // A goed, B leeg
        c.stage_update(); // probeer B, tries=3
        // B crasht steeds (nooit mark_good): drie pogingen, dan rollback.
        assert_eq!(c.on_boot(), Slot::B); // tries 3→2
        assert_eq!(c.on_boot(), Slot::B); // 2→1
        assert_eq!(c.on_boot(), Slot::B); // 1→0
        // Vierde boot: pogingen op, B was Trying → rollback naar A.
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
        c.mark_good(); // nu draait B, A nog goed
        assert!(c.rollback()); // terug naar A
        assert_eq!(c.next_boot, Slot::A);
        // Maar als het andere slot niet goed is, faalt rollback.
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
        // Eén bit flippen → checksum faalt → None (val terug op initial).
        let mut bad = bytes;
        bad[7] ^= 0xFF;
        assert_eq!(SlotConfig::deserialize(&bad), None);
        // Verkeerde magic → None.
        let mut nomagic = bytes;
        nomagic[0] ^= 0xFF;
        assert_eq!(SlotConfig::deserialize(&nomagic), None);
        // Te kort → None.
        assert_eq!(SlotConfig::deserialize(&bytes[..10]), None);
    }

    #[test]
    fn alternating_updates_use_inactive_slot() {
        let mut c = SlotConfig::initial(); // active A
        c.stage_update(); // → B
        c.on_boot();
        c.mark_good(); // active B
        assert_eq!(c.inactive(), Slot::A);
        c.stage_update(); // volgende update gaat naar A
        assert_eq!(c.next_boot, Slot::A);
        c.on_boot();
        c.mark_good();
        assert_eq!(c.active, Slot::A);
        assert_eq!(c.generation, 3); // initial=1, +2 updates
    }
}
