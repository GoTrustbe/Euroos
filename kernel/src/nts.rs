//! 3D-7 — Network Time Security (RFC 8915) boot self-test. Proves the
//! authenticated-time protocol core: a client request + a trusted-server
//! response whose timestamp the client accepts ONLY because it is bound by the
//! AEAD authenticator and the Unique Identifier — and a tampered timestamp or an
//! off-path reply are refused. Nonces/identifiers come from the 3D-8 CSPRNG.

/// `[3d7]` self-test.
pub fn selftest() {
    // In production the C2S/S2C keys come from an NTS-KE handshake over TLS; here
    // we derive them from a CSPRNG-drawn exporter secret to exercise the schedule.
    let mut ems = [0x42u8; 32];
    crate::entropy::getrandom(&mut ems);
    let c2s = euronts::derive_key(&ems, euronts::AEAD_CHACHA20_POLY1305, true);
    let s2c = euronts::derive_key(&ems, euronts::AEAD_CHACHA20_POLY1305, false);

    let mut uid = [0u8; 32];
    let mut n1 = [0u8; 12];
    let mut n2 = [0u8; 12];
    crate::entropy::getrandom(&mut uid);
    crate::entropy::getrandom(&mut n1);
    crate::entropy::getrandom(&mut n2);

    let _req = euronts::client_request(&c2s, b"cookie-boot", uid, n1, 0);
    let server_time = 0xE9F5_1234_0000_0000u64; // demo NTP 64-bit timestamp
    let resp = euronts::server_response(&s2c, &uid, &[b"cookie-next"], n2, 0, server_time);

    // The client accepts the time only because it is authenticated + bound.
    let authentic = euronts::verify_response(&s2c, &uid, &resp)
        .map(|r| r.transmit_ts == server_time && r.cookies.len() == 1)
        .unwrap_or(false);

    // Anti time-shift: flip a byte of the transmit timestamp → rejected.
    let mut bad = resp.clone();
    bad[44] ^= 0xFF;
    let tamper_rejected = matches!(euronts::verify_response(&s2c, &uid, &bad), Err(euronts::NtsError::AuthFailed));

    // Off-path: a reply that does not echo our Unique Identifier → rejected.
    let offpath_rejected =
        matches!(euronts::verify_response(&s2c, &[0u8; 32], &resp), Err(euronts::NtsError::UniqueIdMismatch));

    let ok = authentic && tamper_rejected && offpath_rejected;
    crate::serial_println!(
        "[3d7] EuroNTS authenticated time (RFC 8915, TLS-exporter key schedule, ChaCha20-Poly1305 AEAD): authentic-time-accepted={authentic}, tampered-timestamp-REJECTED={tamper_rejected}, off-path-reply-REJECTED={offpath_rejected} → {}",
        if ok { "OK (the clock trusts only a cryptographically-bound server; live NTS-KE-over-TLS + AES-SIV pending) ✓" } else { "FAILED" }
    );
}
