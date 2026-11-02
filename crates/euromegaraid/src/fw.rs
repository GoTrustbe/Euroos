//! The firmware status word and the handshake that drives it to READY.
//!
//! The firmware reports its state in the top nibble of the status register
//! (OSP0 on gen2), with the number of commands it accepts in the low 16 bits
//! and the scatter-gather limit in bits 16..24. The host moves it on with the
//! inbound doorbell: clear a pending handshake, acknowledge a boot message,
//! take an OPERATIONAL controller (a previous driver) back to READY. Every
//! other state is the firmware's own work, which the host only waits out,
//! with a timeout per state (state values: QEMU `mfi.h`; the sequence as
//! FreeBSD `mfi_transition_firmware` performs it).

use crate::regs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FwState {
    Undefined,
    BbInit,
    FwInit,
    WaitHandshake,
    FwInit2,
    DeviceScan,
    BootMsgPending,
    FlushCache,
    Ready,
    Operational,
    Fault,
    /// A nibble the interface does not define.
    Other(u8),
}

/// The status word as read from the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FwStatus(pub u32);

impl FwStatus {
    pub fn state(self) -> FwState {
        match (self.0 >> 28) as u8 {
            0x0 => FwState::Undefined,
            0x1 => FwState::BbInit,
            0x4 => FwState::FwInit,
            0x6 => FwState::WaitHandshake,
            0x7 => FwState::FwInit2,
            0x8 => FwState::DeviceScan,
            0x9 => FwState::BootMsgPending,
            0xA => FwState::FlushCache,
            0xB => FwState::Ready,
            0xC => FwState::Operational,
            0xF => FwState::Fault,
            n => FwState::Other(n),
        }
    }

    /// Commands the firmware accepts at once (also the reply-queue length).
    pub fn max_cmds(self) -> u32 {
        self.0 & 0xFFFF
    }

    /// Scatter-gather elements per command.
    pub fn max_sge(self) -> u32 {
        (self.0 >> 16) & 0xFF
    }

    pub fn msix_capable(self) -> bool {
        self.0 & 0x0400_0000 != 0
    }
}

/// What the host must do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// READY: the init frame may be sent.
    Done,
    /// Write this value to the inbound doorbell, then keep stepping.
    Doorbell(u32),
    /// Mask interrupts, then write this value to the doorbell (OPERATIONAL -> READY).
    MaskThenDoorbell(u32),
    /// The firmware is busy; read the status again after a short pause.
    Wait,
    /// The firmware reports a fault (the raw status word).
    Fault(u32),
    /// The firmware stayed in this state longer than the timeout.
    Timeout(FwState),
    /// The state changed more often than any real sequence does.
    Flapping,
}

/// The handshake state machine. Feed it every status read with the current
/// time; it issues each doorbell action once per entry into a state and times
/// out a state that does not move.
#[derive(Clone, Debug)]
pub struct Handshake {
    timeout_ms: u64,
    last: Option<FwState>,
    since_ms: u64,
    transitions: u32,
    acted: bool,
}

/// More state changes than this is a misbehaving device.
pub const MAX_TRANSITIONS: u32 = 64;

impl Handshake {
    pub fn new(timeout_ms: u64) -> Handshake {
        Handshake { timeout_ms, last: None, since_ms: 0, transitions: 0, acted: false }
    }

    pub fn transitions(&self) -> u32 {
        self.transitions
    }

    pub fn step(&mut self, raw: u32, now_ms: u64) -> Step {
        let st = FwStatus(raw).state();
        if self.last != Some(st) {
            self.last = Some(st);
            self.since_ms = now_ms;
            self.acted = false;
            self.transitions += 1;
            if self.transitions > MAX_TRANSITIONS {
                return Step::Flapping;
            }
        }
        match st {
            FwState::Ready => return Step::Done,
            FwState::Fault => return Step::Fault(raw),
            _ => {}
        }
        if now_ms.saturating_sub(self.since_ms) > self.timeout_ms {
            return Step::Timeout(st);
        }
        let action = match st {
            FwState::WaitHandshake => Some(Step::Doorbell(regs::IDB_CLEAR_HANDSHAKE | regs::IDB_HOTPLUG)),
            FwState::BootMsgPending => Some(Step::Doorbell(regs::IDB_HOTPLUG)),
            FwState::Operational => Some(Step::MaskThenDoorbell(regs::IDB_RESET_FLAGS)),
            _ => None,
        };
        match action {
            Some(a) if !self.acted => {
                self.acted = true;
                a
            }
            _ => Step::Wait,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const READY_QEMU: u32 = 0xB000_0000 | (80 << 16) | 1008;

    #[test]
    fn decode_status() {
        let s = FwStatus(READY_QEMU | 0x0400_0000);
        assert_eq!(s.state(), FwState::Ready);
        assert_eq!(s.max_cmds(), 1008);
        assert_eq!(s.max_sge(), 80);
        assert!(s.msix_capable());
        assert_eq!(FwStatus(0xF000_0001).state(), FwState::Fault);
        assert_eq!(FwStatus(0x3000_0000).state(), FwState::Other(3));
    }

    #[test]
    fn ready_at_once() {
        let mut h = Handshake::new(1000);
        assert_eq!(h.step(READY_QEMU, 0), Step::Done);
    }

    #[test]
    fn operational_is_reset_once() {
        let mut h = Handshake::new(1000);
        assert_eq!(h.step(0xC000_03F0, 0), Step::MaskThenDoorbell(regs::IDB_RESET_FLAGS));
        // Still operational: no second doorbell, just wait.
        assert_eq!(h.step(0xC000_03F0, 10), Step::Wait);
        assert_eq!(h.step(READY_QEMU, 20), Step::Done);
    }

    #[test]
    fn handshake_and_boot_message() {
        let mut h = Handshake::new(1000);
        assert_eq!(h.step(0x4000_0000, 0), Step::Wait); // FW_INIT: firmware work
        assert_eq!(h.step(0x6000_0000, 5), Step::Doorbell(regs::IDB_CLEAR_HANDSHAKE | regs::IDB_HOTPLUG));
        assert_eq!(h.step(0x6000_0000, 6), Step::Wait);
        assert_eq!(h.step(0x9000_0000, 7), Step::Doorbell(regs::IDB_HOTPLUG));
        assert_eq!(h.step(0x8000_0000, 8), Step::Wait);
        assert_eq!(h.step(READY_QEMU, 9), Step::Done);
        assert_eq!(h.transitions(), 5);
    }

    #[test]
    fn fault_and_timeout() {
        let mut h = Handshake::new(100);
        assert_eq!(h.step(0xF000_1234, 0), Step::Fault(0xF000_1234));
        let mut h = Handshake::new(100);
        assert_eq!(h.step(0x8000_0000, 0), Step::Wait);
        assert_eq!(h.step(0x8000_0000, 100), Step::Wait);
        assert_eq!(h.step(0x8000_0000, 101), Step::Timeout(FwState::DeviceScan));
    }

    #[test]
    fn a_flapping_device_is_refused() {
        let mut h = Handshake::new(1000);
        let mut last = Step::Wait;
        for i in 0..200u64 {
            let raw = if i % 2 == 0 { 0x4000_0000 } else { 0x8000_0000 };
            last = h.step(raw, i);
            if last == Step::Flapping {
                break;
            }
        }
        assert_eq!(last, Step::Flapping);
    }
}
