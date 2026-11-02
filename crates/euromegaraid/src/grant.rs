//! Which part of the register BAR the driver process may map.
//!
//! On MegaRAID gen2 the MSI-X table and its pending-bit array live in the
//! same memory BAR as the MFI registers (QEMU `megasas-gen2`: BAR 1, 16 KiB,
//! table at 0x2000, PBA at 0x3800). A driver that could write the table could
//! aim the device's interrupt messages at any address, so the grant covers the
//! register pages *below* the first MSI-X structure and nothing from there on.
//! The kernel programs the table itself.

use crate::regs::REG_SPAN;

const PAGE: u64 = 4096;

/// Where an MSI-X structure sits: BAR index and byte offset (from the capability).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsixPlace {
    pub bir: u8,
    pub offset: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantError {
    /// The registers the driver needs would overlap the MSI-X table or PBA.
    RegistersOverlapMsix,
    /// The BAR is smaller than the register block.
    BarTooSmall,
    Unaligned,
}

/// The register window `(base, len)` to grant for memory BAR `bar` at
/// `bar_base` with size `bar_len`, excluding every page from the first MSI-X
/// structure (table or PBA) placed in that same BAR.
pub fn register_window(bar: u8, bar_base: u64, bar_len: u64, msix: &[MsixPlace]) -> Result<(u64, u64), GrantError> {
    if bar_base % PAGE != 0 {
        return Err(GrantError::Unaligned);
    }
    if bar_len < REG_SPAN as u64 {
        return Err(GrantError::BarTooSmall);
    }
    let mut end = bar_len;
    for m in msix.iter().filter(|m| m.bir == bar) {
        end = end.min(m.offset & !(PAGE - 1));
    }
    if end < REG_SPAN as u64 {
        return Err(GrantError::RegistersOverlapMsix);
    }
    let len = end.div_ceil(PAGE) * PAGE;
    Ok((bar_base, len.min(bar_len.div_ceil(PAGE) * PAGE)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_gen2_layout() {
        // BAR 1, 16 KiB, table at 0x2000, PBA at 0x3800 (QEMU megasas_scsi_realize).
        let m = [MsixPlace { bir: 1, offset: 0x2000 }, MsixPlace { bir: 1, offset: 0x3800 }];
        assert_eq!(register_window(1, 0xFEB8_0000, 0x4000, &m), Ok((0xFEB8_0000, 0x2000)));
    }

    #[test]
    fn dell_perc_h730_layout() {
        // The real H730 Mini (SAS3108, read with `lspci -vv` on the test host):
        // BAR 1 64 KiB, MSI-X table at 0xE000, PBA at 0xF000.
        let m = [MsixPlace { bir: 1, offset: 0xE000 }, MsixPlace { bir: 1, offset: 0xF000 }];
        assert_eq!(register_window(1, 0x91D0_0000, 0x1_0000, &m), Ok((0x91D0_0000, 0xE000)));
    }

    #[test]
    fn msix_elsewhere_grants_the_whole_bar() {
        let m = [MsixPlace { bir: 3, offset: 0 }];
        assert_eq!(register_window(1, 0xFEB8_0000, 0x4000, &m), Ok((0xFEB8_0000, 0x4000)));
        assert_eq!(register_window(1, 0xFEB8_0000, 0x4000, &[]), Ok((0xFEB8_0000, 0x4000)));
    }

    #[test]
    fn table_inside_a_page_excludes_that_page() {
        // Table at 0x1800: the page 0x1000.. is shared with it, so it is not granted.
        let m = [MsixPlace { bir: 0, offset: 0x1800 }];
        assert_eq!(register_window(0, 0x1_0000_0000, 0x1_0000, &m), Ok((0x1_0000_0000, 0x1000)));
    }

    #[test]
    fn refusals() {
        let m = [MsixPlace { bir: 1, offset: 0x80 }];
        assert_eq!(register_window(1, 0xFEB8_0000, 0x4000, &m), Err(GrantError::RegistersOverlapMsix));
        assert_eq!(register_window(1, 0xFEB8_0000, 0x80, &[]), Err(GrantError::BarTooSmall));
        assert_eq!(register_window(1, 0xFEB8_0010, 0x4000, &[]), Err(GrantError::Unaligned));
    }
}
