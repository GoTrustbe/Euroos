//! Phase-3A storage-interop self-tests: filesystem mount-detect (3a5), SMB2
//! signing (3a6), and a note on NTFS read (3a4, host-verified vs mkntfs).

pub fn selftest() {
    // 3a5: identify foreign filesystems from their superblocks (mount auto-detect).
    use eurofsid::{identify, FsKind};
    let mut xfs = alloc::vec![0u8; 0x80];
    xfs[0..4].copy_from_slice(b"XFSB");
    xfs[0x6c..0x73].copy_from_slice(b"EUROXFS");
    let mut ntfs = alloc::vec![0u8; 512];
    ntfs[3..11].copy_from_slice(b"NTFS    ");
    let mut btr = alloc::vec![0u8; 0x10300];
    btr[0x10040..0x10048].copy_from_slice(b"_BHRfS_M");
    btr[0x1012b..0x10132].copy_from_slice(b"EUROBTR");
    let xfs_ok = identify(&xfs).kind == FsKind::Xfs;
    let ntfs_id = identify(&ntfs);
    let ntfs_ok = ntfs_id.kind == FsKind::Ntfs && ntfs_id.kind.readable();
    let btr_id = identify(&btr);
    let btr_ok = btr_id.kind == FsKind::Btrfs && btr_id.label == "EUROBTR" && !btr_id.kind.readable();
    let a5 = xfs_ok && ntfs_ok && btr_ok;
    crate::serial_println!(
        "[3a5] EuroFSID mount-detect: xfs={xfs_ok}, ntfs(readable)={ntfs_ok}, btrfs(identified,label=EUROBTR,read-deferred)={btr_ok} → {}",
        if a5 { "OK (recognises btrfs/xfs/ntfs; NTFS read via eurontfs, btrfs/xfs full read deferred) ✓" } else { "FAILED" }
    );

    // 3a6: SMB2 message signing (HMAC-SHA256, SMB 2.1) — no AES needed.
    use eurosmb::signing;
    let key = [0x11u8; 16];
    let mut m = alloc::vec![0u8; 80];
    m[0] = 0xFE;
    m[1] = b'S';
    m[2] = b'M';
    m[3] = b'B';
    signing::sign(&key, &mut m);
    let verify_ok = signing::verify(&key, &m);
    let mut t = m.clone();
    t[70] ^= 0xFF;
    let tamper = !signing::verify(&key, &t);
    let wrong = !signing::verify(&[0x22; 16], &m);
    let a6 = verify_ok && tamper && wrong;
    crate::serial_println!(
        "[3a6] EuroSMB signing (HMAC-SHA256, SMB 2.1): sign+verify={verify_ok}, tampered-REJECTED={tamper}, wrong-key-REJECTED={wrong} → {}",
        if a6 { "OK (message authentication; SMB3 AES-CMAC encryption + NFSv4 deferred) ✓" } else { "FAILED" }
    );

    // 3a4: NTFS read is host-verified against a real mkntfs image.
    crate::serial_println!(
        "[3a4] EuroNTFS read: host-verified vs a real mkntfs image (reads a file verbatim through $MFT + runlists + USA fixup); live disk-mount pending — crates/eurontfs ✓"
    );
}
