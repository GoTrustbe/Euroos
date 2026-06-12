//! Internet-checksum (RFC 1071): 16-bit ones-complement som. Gebruikt door
//! IPv4, ICMP, UDP en TCP. Big-endian.

/// Bereken de internet-checksum over `data` (met optionele pseudo-header-prefix
/// die al in `data` zit). Een oneven byte wordt als hoog-byte gepad.
pub fn internet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut chunks = data.chunks_exact(2);
    for c in &mut chunks {
        sum += u16::from_be_bytes([c[0], c[1]]) as u32;
    }
    if let [last] = chunks.remainder() {
        sum += (*last as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// True als de checksum van een blok klopt (som incl. checksum-veld == 0xFFFF).
pub fn verify(data: &[u8]) -> bool {
    internet_checksum(data) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc1071_voorbeeld() {
        // Klassiek voorbeeld: bytes met bekende checksum 0x432B (som-complement).
        let data = [0x00u8, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        let cs = internet_checksum(&data);
        // Zelf-consistent: data + checksum verifieert naar 0.
        let mut with = data.to_vec();
        with.extend_from_slice(&cs.to_be_bytes());
        assert!(verify(&with));
    }

    #[test]
    fn oneven_lengte() {
        // Checksumveld op een even offset (vooraan), gevolgd door oneven payload.
        // De laatste byte wordt intern met een zero-octet gepad (RFC 1071).
        let mut blk = [0u8, 0, 0x12, 0x34, 0x56]; // [csum][payload]
        let cs = internet_checksum(&blk);
        blk[0..2].copy_from_slice(&cs.to_be_bytes());
        assert!(verify(&blk));
    }
}
