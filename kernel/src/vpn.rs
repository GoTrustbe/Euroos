//! Kernel side of **EuroVPN** (plan N2): at boot, set up a sovereign, forward-
//! secret tunnel between our (TPM-seeded) identity and a peer, and prove an
//! encrypted round-trip. The handshake/transport crypto is host-tested in
//! [`eurovpn`]; here we demonstrate it live no_std + store our public key.

use alloc::string::String;
use alloc::vec::Vec;

use eurovpn::Identity;
use spin::Mutex;

static LOCAL_PUB: Mutex<[u8; 32]> = Mutex::new([0u8; 32]);

fn hex8(b: &[u8]) -> String {
    let mut s = String::new();
    for &x in b.iter().take(8) {
        s.push_str(&alloc::format!("{x:02x}"));
    }
    s
}

/// Boot self-test: full handshake (initiator + responder) + transport round-trip.
/// `s_local`/`s_peer`/`e_init`/`e_resp` are 32-byte seeds (from the TPM RNG).
pub fn selftest(s_local: [u8; 32], s_peer: [u8; 32], e_init: [u8; 32], e_resp: [u8; 32], from_tpm: bool) {
    let local = Identity::from_seed(s_local);
    let peer = Identity::from_seed(s_peer);
    *LOCAL_PUB.lock() = local.public;

    // We initiate towards the peer; the peer responds.
    let (our_eph, pending) = eurovpn::initiate(&local, peer.public, e_init);
    let (peer_eph, mut peer_tun) = eurovpn::respond(&peer, local.public, our_eph, e_resp);
    let mut our_tun = pending.finish(peer_eph);

    // Encrypted round-trip over the tunnel.
    let msg = b"EuroOS sovereign VPN tunnel";
    let ct = our_tun.encrypt(msg);
    let encrypted = &ct[8..] != &msg[..];
    let decrypted_ok = peer_tun.decrypt(&ct).map(|d| d == msg).unwrap_or(false);
    let reply = peer_tun.encrypt(b"ack");
    let reply_ok = our_tun.decrypt(&reply).map(|d| d == b"ack").unwrap_or(false);

    let ok = encrypted && decrypted_ok && reply_ok;
    crate::serial_println!(
        "[n2] EuroVPN: handshake (4×X25519-DH → HKDF) + ChaCha20-Poly1305 transport, seeds-from-TPM={from_tpm}, pubkey {}…, encrypted={encrypted}, peer-decrypts={decrypted_ok}, reply-ok={reply_ok} → {}",
        hex8(&local.public),
        if ok { "OK (forward-secret, mutually-authenticated tunnel) ✓" } else { "FAILED" }
    );
}

/// 3D-9 boot self-test: the **hybrid post-quantum** handshake (X25519 +
/// ML-KEM-768) + transport round-trip, and proof that the ML-KEM secret is
/// load-bearing (a tampered KEM ciphertext breaks the tunnel).
pub fn selftest_hybrid(
    s_local: [u8; 32],
    s_peer: [u8; 32],
    e_init: [u8; 32],
    e_resp: [u8; 32],
    kem_seed: [u8; 32],
    kem_rand: [u8; 32],
    from_tpm: bool,
) {
    let local = Identity::from_seed(s_local);
    let peer = Identity::from_seed(s_peer);

    // Full hybrid handshake + encrypted round-trip.
    let (our_eph, ek, pending) = eurovpn::initiate_hybrid(&local, peer.public, e_init, kem_seed);
    let (peer_eph, ct, mut peer_tun) = eurovpn::respond_hybrid(&peer, local.public, our_eph, &ek, e_resp, kem_rand);
    let mut our_tun = pending.finish(peer_eph, &ct);
    let msg = b"EuroOS post-quantum VPN tunnel";
    let enc = our_tun.encrypt(msg);
    let encrypted = enc[8..] != msg[..];
    let peer_decrypts = peer_tun.decrypt(&enc).map(|d| d == msg).unwrap_or(false);
    let reply = peer_tun.encrypt(b"pq-ack");
    let reply_ok = our_tun.decrypt(&reply).map(|d| d == b"pq-ack").unwrap_or(false);

    // Prove the PQ secret is real: a tampered KEM ciphertext must break agreement.
    let (our_eph2, ek2, pend2) = eurovpn::initiate_hybrid(&local, peer.public, e_init, kem_seed);
    let (peer_eph2, mut ct2, mut peer_tun2) = eurovpn::respond_hybrid(&peer, local.public, our_eph2, &ek2, e_resp, kem_rand);
    ct2[0] ^= 0xFF;
    let mut our_tun2 = pend2.finish(peer_eph2, &ct2);
    let e2 = our_tun2.encrypt(b"x");
    let tampered_breaks = peer_tun2.decrypt(&e2).is_none();

    let ok = encrypted && peer_decrypts && reply_ok && tampered_breaks;
    crate::serial_println!(
        "[3d9] EuroVPN hybrid PQC: X25519 + ML-KEM-768 (FIPS 203, NIST-KAT-verified), ek={}B ct={}B, seeds-from-TPM={from_tpm}, encrypted={encrypted}, peer-decrypts={peer_decrypts}, reply-ok={reply_ok}, tampered-KEM-breaks-tunnel={tampered_breaks} → {}",
        ek.len(), ct.len(),
        if ok { "OK (secret if EITHER X25519 or ML-KEM stands — harvest-now-decrypt-later resistant) ✓" } else { "FAILED" }
    );
}

/// `vpn` shell command: show our public tunnel key.
pub fn shell() -> Vec<String> {
    let pub_full: String = LOCAL_PUB.lock().iter().map(|x| alloc::format!("{x:02x}")).collect();
    alloc::vec![
        String::from("EuroVPN — sovereign, forward-secret VPN (X25519 + HKDF-SHA256 + ChaCha20-Poly1305)"),
        alloc::format!("  local public key: {pub_full}"),
        String::from("  share this with a peer (like a WireGuard config) to set up a tunnel"),
    ]
}
