//! MFI register offsets, doorbell bits and the controller families.
//!
//! Offsets are hardware facts (QEMU `hw/scsi/mfi.h`, FreeBSD `mfireg.h`). All
//! registers are 32 bits wide and live in the memory BAR (BAR 1 on gen2).

/// Inbound doorbell: the host drives the firmware state machine through it.
pub const IDB: u32 = 0x20;
/// Outbound interrupt status (gen2: bit 0 = reply message).
pub const OSTS: u32 = 0x30;
/// Outbound interrupt mask. All ones = every interrupt masked.
pub const OMSK: u32 = 0x34;
/// Inbound queue port: post a frame (`addr | extra_frames << 1 | 1`).
pub const IQP: u32 = 0x40;
/// Outbound doorbell clear (write the OSTS value back to acknowledge).
pub const ODCR0: u32 = 0xA0;
/// Outbound scratch pad 0: the firmware status word on gen2.
pub const OSP0: u32 = 0xB0;
/// Outbound message 0: the firmware status word on older parts (same value in QEMU).
pub const OMSG0: u32 = 0x18;

/// Highest register offset the driver touches, plus its width.
pub const REG_SPAN: u32 = 0x100;

/// IDB bits.
pub const IDB_ABORT: u32 = 0x01;
pub const IDB_READY: u32 = 0x02;
pub const IDB_MFIMODE: u32 = 0x04;
pub const IDB_CLEAR_HANDSHAKE: u32 = 0x08;
pub const IDB_HOTPLUG: u32 = 0x10;
/// From OPERATIONAL back to READY: abort everything, ready, MFI mode.
pub const IDB_RESET_FLAGS: u32 = IDB_READY | IDB_MFIMODE | IDB_ABORT;

/// Gen2 interrupt bits in OSTS/OMSK.
pub const GEN2_REPLY: u32 = 0x0000_0001;
pub const GEN2_ENABLE_MASK: u32 = 0x0000_0005;
/// Value for OMSK that unmasks the gen2 reply interrupt.
pub const GEN2_UNMASK: u32 = !GEN2_ENABLE_MASK;
/// Value for OMSK that masks everything.
pub const MASK_ALL: u32 = 0xFFFF_FFFF;

/// The controller family, from the PCI ids. Only [`Family::MfiGen2`] is
/// driven; the others are named so the driver can say what it found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// SAS1078 (`1000:0060`): MFI, "ppc" register semantics. Not driven.
    Mfi1078,
    /// SAS2108 (`1000:0078`, `1000:0079`): MFI gen2. QEMU `megasas-gen2`.
    MfiGen2,
    /// SAS2008-based "skinny" MFI (`1000:0071`, `1000:0073`). Not driven.
    MfiSkinny,
    /// Fusion (MPT request descriptors): 2208, 3108, 3008 and later. Not driven.
    Fusion(&'static str),
    Unknown,
}

/// Classify a PCI function.
pub fn family(vendor: u16, device: u16) -> Family {
    if vendor != 0x1000 {
        return Family::Unknown;
    }
    match device {
        0x0060 => Family::Mfi1078,
        0x0078 | 0x0079 => Family::MfiGen2,
        0x0071 | 0x0073 => Family::MfiSkinny,
        0x005b => Family::Fusion("SAS2208 Thunderbolt"),
        0x005d => Family::Fusion("SAS3108 Invader (Dell PERC H730/H730P/H830)"),
        0x005f => Family::Fusion("SAS3008 Fury"),
        0x00ce | 0x00cf => Family::Fusion("SAS3516/3416 Ventura"),
        0x10e0..=0x10e7 => Family::Fusion("SAS3916/3816 Aero"),
        _ => Family::Unknown,
    }
}

/// The value written to [`IQP`] to post a frame, or `None` when the frame
/// address cannot be expressed (not 32-byte aligned, or above 4 GiB: gen2
/// posts through the 32-bit queue port only).
pub fn post_value(frame_addr: u64, extra_frames: u8) -> Option<u32> {
    if frame_addr & 0x1F != 0 || frame_addr > u32::MAX as u64 || extra_frames > 0xF {
        return None;
    }
    Some(frame_addr as u32 | ((extra_frames as u32) << 1) | 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families() {
        assert_eq!(family(0x1000, 0x0079), Family::MfiGen2);
        assert_eq!(family(0x1000, 0x0078), Family::MfiGen2);
        assert!(matches!(family(0x1000, 0x005d), Family::Fusion(n) if n.contains("H730")));
        assert!(matches!(family(0x1000, 0x005b), Family::Fusion(_)));
        assert_eq!(family(0x1000, 0x0060), Family::Mfi1078);
        assert_eq!(family(0x8086, 0x0079), Family::Unknown);
        assert_eq!(family(0x1000, 0x1234), Family::Unknown);
    }

    #[test]
    fn post_values() {
        assert_eq!(post_value(0x0012_3400, 0), Some(0x0012_3401));
        assert_eq!(post_value(0x0012_3400, 2), Some(0x0012_3405));
        assert_eq!(post_value(0x0012_3410, 0), None); // not 32-byte aligned
        assert_eq!(post_value(0x1_0000_0000, 0), None); // above 4 GiB
        assert_eq!(post_value(0xFFFF_FFE0, 0), Some(0xFFFF_FFE1));
        assert_eq!(post_value(0x1000, 16), None);
    }

    #[test]
    fn masks() {
        assert_eq!(GEN2_UNMASK, 0xFFFF_FFFA);
        assert_ne!(GEN2_UNMASK, MASK_ALL); // QEMU treats all-ones as "disabled"
        assert_eq!(IDB_RESET_FLAGS, 0x7);
    }
}
