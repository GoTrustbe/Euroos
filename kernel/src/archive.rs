//! Kernel-zijde van **EuroArchive** (Sprint AC-2): de archiefbeheerder.
//! Bij boot bewijzen we de USTAR tar-round-trip + checksum-verificatie en de
//! Ed25519-manifest-haak. Host-geteste kern: [`euroarchive`].

use crate::serial_println;
use euroarchive::{read_tar, verify_manifest, write_tar, ArchiveError, Entry, Kind};

/// Boot-zelftest: pak een archief in en weer uit, detecteer corruptie.
pub fn selftest() {
    let entries = [
        Entry::dir("euro"),
        Entry::file("euro/hallo.txt", b"Hallo EuroOS"),
        Entry::file("euro/data.bin", &[0u8, 1, 2, 3, 255, 254]),
    ];
    let tar = write_tar(&entries);
    let back = read_tar(&tar);

    let roundtrip_ok = match &back {
        Ok(v) => {
            v.len() == 3
                && v[0].kind == Kind::Dir
                && v[1].name == "euro/hallo.txt"
                && v[1].data == b"Hallo EuroOS"
                && v[2].data == [0u8, 1, 2, 3, 255, 254]
        }
        Err(_) => false,
    };

    // Corruptie-detectie: verander een headerbyte → checksum-fout.
    let mut corrupt = tar.clone();
    corrupt[1] = b'X';
    let corrupt_caught = matches!(read_tar(&corrupt), Err(ArchiveError::BadChecksum { .. }));

    // Manifest-haak: alleen het juiste "handtekening"-paar verifieert.
    let v = back.unwrap_or_default();
    let manifest = [
        (alloc::string::String::from("euro/hallo.txt"), b"Hallo EuroOS".to_vec()),
        (alloc::string::String::from("euro/data.bin"), b"fout".to_vec()),
    ];
    let verified = verify_manifest(&v, &manifest, |data, sig| data == sig);
    let manifest_ok = verified.len() == 1 && verified[0] == "euro/hallo.txt";

    let ok = roundtrip_ok && corrupt_caught && manifest_ok;
    serial_println!(
        "[az] EuroArchive: tar {} bytes, round-trip={}, corruptie-gedetecteerd={}, manifest-verify={}/2 {}",
        tar.len(),
        roundtrip_ok,
        corrupt_caught,
        verified.len(),
        if ok { "✓" } else { "✗ FOUT" }
    );
}
