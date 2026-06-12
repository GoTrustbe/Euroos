//! Kernel-zijde van **EuroVPN** (plan N2): zet bij boot een soevereine, forward-
//! secret tunnel op tussen onze (TPM-geseede) identiteit en een peer, en bewijs een
//! versleutelde round-trip. De handshake-/transport-crypto zit host-getest in
//! [`eurovpn`]; hier tonen we 'm live no_std + bewaren we onze publieke sleutel.

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

/// Boot-zelftest: volledige handshake (initiator + responder) + transport-round-trip.
/// `s_local`/`s_peer`/`e_init`/`e_resp` zijn 32-byte seeds (van de TPM-RNG).
pub fn selftest(s_local: [u8; 32], s_peer: [u8; 32], e_init: [u8; 32], e_resp: [u8; 32], from_tpm: bool) {
    let local = Identity::from_seed(s_local);
    let peer = Identity::from_seed(s_peer);
    *LOCAL_PUB.lock() = local.public;

    // Wij initiëren naar de peer; de peer antwoordt.
    let (our_eph, pending) = eurovpn::initiate(&local, peer.public, e_init);
    let (peer_eph, mut peer_tun) = eurovpn::respond(&peer, local.public, our_eph, e_resp);
    let mut our_tun = pending.finish(peer_eph);

    // Versleutelde round-trip over de tunnel.
    let msg = b"EuroOS soevereine VPN-tunnel";
    let ct = our_tun.encrypt(msg);
    let encrypted = &ct[8..] != &msg[..];
    let decrypted_ok = peer_tun.decrypt(&ct).map(|d| d == msg).unwrap_or(false);
    let reply = peer_tun.encrypt(b"ack");
    let reply_ok = our_tun.decrypt(&reply).map(|d| d == b"ack").unwrap_or(false);

    let ok = encrypted && decrypted_ok && reply_ok;
    crate::serial_println!(
        "[n2] EuroVPN: handshake (4×X25519-DH → HKDF) + ChaCha20-Poly1305-transport, seeds-van-TPM={from_tpm}, pubkey {}…, versleuteld={encrypted}, peer-ontsleutelt={decrypted_ok}, antwoord-ok={reply_ok} → {}",
        hex8(&local.public),
        if ok { "OK (forward-secret, wederzijds-geauthenticeerde tunnel) ✓" } else { "MISLUKT" }
    );
}

/// `vpn`-shellcommando: toon onze publieke tunnelsleutel.
pub fn shell() -> Vec<String> {
    let pub_full: String = LOCAL_PUB.lock().iter().map(|x| alloc::format!("{x:02x}")).collect();
    alloc::vec![
        String::from("EuroVPN — soevereine, forward-secret VPN (X25519 + HKDF-SHA256 + ChaCha20-Poly1305)"),
        alloc::format!("  lokale publieke sleutel: {pub_full}"),
        String::from("  deel deze met een peer (zoals een WireGuard-config) om een tunnel op te zetten"),
    ]
}
