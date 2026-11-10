//! Identify Namespace data (NVMe 1.4, figure 249) → the geometry a block driver
//! needs, with every field checked. The 4 KiB page comes from the device: a
//! controller that answers with zeros, with an LBA format the driver cannot
//! handle or with a size that does not fit an address is skipped, not trusted.

/// What a namespace looks like to a block driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Namespace size in logical blocks (NSZE).
    pub sectors: u64,
    /// Bytes per logical block (512 ... 4096).
    pub sector_size: u32,
}

/// Why Identify Namespace data was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentifyError {
    /// Fewer bytes than the fixed part with its 16 LBA formats (192).
    Short(usize),
    /// NSZE is 0: the namespace is not active (or the page is empty).
    NoCapacity,
    /// The current LBA format has a data size other than 2^9 ... 2^12 bytes.
    BadSectorSize(u8),
    /// `sectors x sector_size` does not fit in 64 bits.
    TooLarge,
}

/// Bytes of the Identify Namespace structure this parser reads.
pub const IDENTIFY_NS_MIN: usize = 192;

/// Parse Identify Namespace data.
pub fn parse_identify_namespace(data: &[u8]) -> Result<Geometry, IdentifyError> {
    if data.len() < IDENTIFY_NS_MIN {
        return Err(IdentifyError::Short(data.len()));
    }
    let mut nsze = [0u8; 8];
    nsze.copy_from_slice(&data[0..8]);
    let sectors = u64::from_le_bytes(nsze);
    if sectors == 0 {
        return Err(IdentifyError::NoCapacity);
    }
    // FLBAS bits 3:0 select one of the 16 LBA formats at offset 128 (4 bytes
    // each); byte 2 of a format is LBADS, log2 of the block size.
    let flbas = (data[26] & 0xF) as usize;
    let lbads = data[128 + flbas * 4 + 2];
    if !(9..=12).contains(&lbads) {
        return Err(IdentifyError::BadSectorSize(lbads));
    }
    let sector_size = 1u32 << lbads;
    if sectors.checked_mul(sector_size as u64).is_none() {
        return Err(IdentifyError::TooLarge);
    }
    Ok(Geometry { sectors, sector_size })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(sectors: u64, flbas: u8, lbads: u8) -> Vec<u8> {
        let mut d = vec![0u8; 4096];
        d[0..8].copy_from_slice(&sectors.to_le_bytes());
        d[26] = flbas;
        d[128 + (flbas as usize & 0xF) * 4 + 2] = lbads;
        d
    }

    #[test]
    fn a_normal_namespace_parses() {
        assert_eq!(
            parse_identify_namespace(&page(2_097_152, 0, 9)),
            Ok(Geometry { sectors: 2_097_152, sector_size: 512 })
        );
        // Format 15 selected, 4 KiB blocks; the upper FLBAS bits are flags.
        assert_eq!(
            parse_identify_namespace(&page(1000, 0x1F, 12)),
            Ok(Geometry { sectors: 1000, sector_size: 4096 })
        );
    }

    #[test]
    fn broken_identify_data_is_refused() {
        // An all-zero page: what a controller that does not implement the
        // command, or a failed DMA transfer, leaves behind.
        assert_eq!(parse_identify_namespace(&[0u8; 4096]), Err(IdentifyError::NoCapacity));
        // All ones: a dead bus.
        assert_eq!(parse_identify_namespace(&[0xFFu8; 4096]), Err(IdentifyError::BadSectorSize(0xFF)));
        // Block sizes the driver cannot do I/O with.
        assert_eq!(parse_identify_namespace(&page(100, 0, 0)), Err(IdentifyError::BadSectorSize(0)));
        assert_eq!(parse_identify_namespace(&page(100, 0, 8)), Err(IdentifyError::BadSectorSize(8)));
        assert_eq!(parse_identify_namespace(&page(100, 3, 16)), Err(IdentifyError::BadSectorSize(16)));
        // A size no address can reach.
        assert_eq!(parse_identify_namespace(&page(u64::MAX, 0, 12)), Err(IdentifyError::TooLarge));
        // A short buffer.
        assert_eq!(parse_identify_namespace(&[0u8; 100]), Err(IdentifyError::Short(100)));
        assert_eq!(parse_identify_namespace(&[]), Err(IdentifyError::Short(0)));
    }

    #[test]
    fn no_byte_pattern_panics() {
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut d = vec![0u8; 4096];
        for _ in 0..20_000 {
            for b in d.iter_mut().take(IDENTIFY_NS_MIN) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *b = x as u8;
            }
            if let Ok(g) = parse_identify_namespace(&d) {
                assert!(g.sectors > 0 && (512..=4096).contains(&g.sector_size));
            }
        }
    }
}
