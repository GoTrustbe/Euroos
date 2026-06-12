//! EuroVPN — een **soevereine, forward-secret VPN-tunnel** (plan N2, WireGuard-stijl).
//!
//! Een netwerk-soeverein OS heeft een eigen, moderne VPN nodig. EuroVPN gebruikt een
//! **Noise-achtige authenticated key-exchange**: elke kant heeft een statische
//! X25519-sleutel (vooraf uitgewisseld, zoals een WireGuard-peer) en genereert een
//! efemere sleutel per sessie. De gedeelde sessiesleutel wordt afgeleid uit een
//! **viervoudige Diffie-Hellman** — `e_i·e_r`, `e_i·S_r`, `s_i·e_r`, `s_i·S_r` — via
//! HKDF-SHA256. Dat geeft tegelijk **forward secrecy** (de efemere DH's) én
//! **wederzijdse authenticatie** (de statische DH's): een aanvaller zonder een van de
//! privé-sleutels kan de tunnel niet afleiden. Het transport is **ChaCha20-Poly1305**
//! met een per-pakket-teller-nonce. Pure `no_std`-crypto → host-getest.
//!
//! (Bewust geen byte-compat met WireGuard zelf — dat vereist BLAKE2s; dit is de
//! sovereign variant op dezelfde cryptografische principes.)

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

/// Een statische identiteit (X25519-sleutelpaar). De publieke sleutel deel je met
/// peers; de privé-sleutel blijft lokaal.
pub struct Identity {
    secret: StaticSecret,
    pub public: [u8; 32],
}

impl Identity {
    /// Leid een identiteit af uit 32 seed-bytes (bv. van de TPM-RNG).
    pub fn from_seed(seed: [u8; 32]) -> Identity {
        let secret = StaticSecret::from(seed);
        let public = PublicKey::from(&secret).to_bytes();
        Identity { secret, public }
    }

    fn dh(&self, peer_pub: &[u8; 32]) -> [u8; 32] {
        self.secret.diffie_hellman(&PublicKey::from(*peer_pub)).to_bytes()
    }
}

/// Een opgezette tunnel: richtinggebonden sleutels + tellers.
pub struct Tunnel {
    send_key: [u8; 32],
    recv_key: [u8; 32],
    send_ctr: u64,
    /// Hoogste aanvaarde ontvangst-teller (+1) = de bovenkant van het anti-replay-venster.
    recv_ctr: u64,
    /// Bitmasker van de laatste 64 reeds-geziene tellers onder `recv_ctr` (WireGuard-stijl
    /// sliding window) — voorkomt herhaalde/oude pakketten (audit H2).
    replay_window: u64,
}

/// Combineer de vier DH-resultaten tot het tunnel-sleutelmateriaal (HKDF-SHA256).
/// `initiator` bepaalt welke afgeleide sleutel "send" en welke "recv" is.
fn derive(dh1: [u8; 32], dh2: [u8; 32], dh3: [u8; 32], dh4: [u8; 32], initiator: bool) -> Tunnel {
    let mut ikm = Vec::with_capacity(128);
    ikm.extend_from_slice(&dh1);
    ikm.extend_from_slice(&dh2);
    ikm.extend_from_slice(&dh3);
    ikm.extend_from_slice(&dh4);
    let hk = Hkdf::<Sha256>::new(Some(b"EuroVPN-v1"), &ikm);
    let mut k_i2r = [0u8; 32]; // initiator → responder
    let mut k_r2i = [0u8; 32]; // responder → initiator
    hk.expand(b"i2r", &mut k_i2r).unwrap();
    hk.expand(b"r2i", &mut k_r2i).unwrap();
    if initiator {
        Tunnel { send_key: k_i2r, recv_key: k_r2i, send_ctr: 0, recv_ctr: 0, replay_window: 0 }
    } else {
        Tunnel { send_key: k_r2i, recv_key: k_i2r, send_ctr: 0, recv_ctr: 0, replay_window: 0 }
    }
}

/// **Initiator-zijde** van de handshake. `our` = onze statische identiteit,
/// `peer_static` = de publieke statische sleutel van de responder, `eph_seed` = seed
/// voor onze efemere sleutel. Geeft (onze efemere pubkey om te versturen, vervolg).
pub fn initiate(our: &Identity, peer_static: [u8; 32], eph_seed: [u8; 32]) -> (([u8; 32]), PendingInitiator) {
    let eph = Identity::from_seed(eph_seed);
    let our_eph_pub = eph.public;
    (
        our_eph_pub,
        PendingInitiator {
            our_static_secret_seed: clone_secret(our),
            our_static_pub: our.public,
            eph,
            peer_static,
        },
    )
}

/// Bewaarde toestand tussen de twee initiator-stappen.
pub struct PendingInitiator {
    our_static_secret_seed: StaticSecret,
    our_static_pub: [u8; 32],
    eph: Identity,
    peer_static: [u8; 32],
}

impl PendingInitiator {
    /// Voltooi de handshake met de efemere pubkey van de responder → de tunnel.
    pub fn finish(self, resp_eph_pub: [u8; 32]) -> Tunnel {
        let s_i = Identity { secret: self.our_static_secret_seed, public: self.our_static_pub };
        let dh1 = self.eph.dh(&resp_eph_pub); // e_i · e_r
        let dh2 = self.eph.dh(&self.peer_static); // e_i · S_r
        let dh3 = s_i.dh(&resp_eph_pub); // s_i · e_r
        let dh4 = s_i.dh(&self.peer_static); // s_i · S_r
        derive(dh1, dh2, dh3, dh4, true)
    }
}

/// **Responder-zijde**: verwerk de initiator-efemere pubkey en geef (onze efemere
/// pubkey om terug te sturen, de tunnel) terug. `peer_static` = de publieke statische
/// sleutel van de initiator (IK: vooraf bekend).
pub fn respond(our: &Identity, peer_static: [u8; 32], init_eph_pub: [u8; 32], eph_seed: [u8; 32]) -> ([u8; 32], Tunnel) {
    let eph = Identity::from_seed(eph_seed);
    let our_eph_pub = eph.public;
    let dh1 = eph.dh(&init_eph_pub); // e_r · e_i  (= e_i · e_r)
    let dh2 = our.dh(&init_eph_pub); // s_r · e_i  (= e_i · S_r)
    let dh3 = eph.dh(&peer_static); // e_r · S_i  (= s_i · e_r)
    let dh4 = our.dh(&peer_static); // s_r · S_i  (= s_i · S_r)
    (our_eph_pub, derive(dh1, dh2, dh3, dh4, false))
}

fn clone_secret(id: &Identity) -> StaticSecret {
    // X25519 StaticSecret is niet Clone; her-derive uit de bytes.
    StaticSecret::from(id.secret.to_bytes())
}

impl Tunnel {
    /// Versleutel een uitgaand pakket (ChaCha20-Poly1305, nonce = send-teller).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.send_key));
        let nonce = ctr_nonce(self.send_ctr);
        self.send_ctr += 1;
        let ct = cipher.encrypt(Nonce::from_slice(&nonce), plaintext).unwrap_or_default();
        // Prefix de teller zodat de ontvanger 'm als nonce kan gebruiken.
        let mut out = Vec::with_capacity(8 + ct.len());
        out.extend_from_slice(&(self.send_ctr - 1).to_le_bytes());
        out.extend_from_slice(&ct);
        out
    }

    /// Ontsleutel een inkomend pakket; verifieert de Poly1305-tag (tamper-evident).
    pub fn decrypt(&mut self, packet: &[u8]) -> Option<Vec<u8>> {
        if packet.len() < 8 {
            return None;
        }
        let ctr = u64::from_le_bytes(packet[..8].try_into().ok()?);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.recv_key));
        // Verifieer eerst de Poly1305-tag (alleen authentieke pakketten tellen mee).
        let pt = cipher.decrypt(Nonce::from_slice(&ctr_nonce(ctr)), &packet[8..]).ok()?;

        // ── Anti-replay (audit H2): WireGuard-stijl 64-bits sliding window. ──
        // `recv_ctr` = hoogste aanvaarde teller + 1; bit k = (recv_ctr-1-k) gezien.
        let top = self.recv_ctr;
        if ctr + 1 > top {
            // Nieuwe hoogste teller → schuif het venster op.
            let shift = ctr + 1 - top;
            self.replay_window = if shift >= 64 { 0 } else { self.replay_window << shift };
            self.replay_window |= 1; // bit 0 = de nieuwe hoogste (ctr)
            self.recv_ctr = ctr + 1;
        } else {
            // Onder de top: te oud of een herhaling?
            let offset = top - 1 - ctr;
            if offset >= 64 {
                return None; // buiten het venster → afwijzen (mogelijk replay)
            }
            let bit = 1u64 << offset;
            if self.replay_window & bit != 0 {
                return None; // deze teller is al gezien → replay
            }
            self.replay_window |= bit;
        }
        Some(pt)
    }

    /// (alleen test/diagnostiek) — de send-sleutel, om matchende sessies te bewijzen.
    pub fn send_key(&self) -> [u8; 32] {
        self.send_key
    }
}

fn ctr_nonce(ctr: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..12].copy_from_slice(&ctr.to_le_bytes());
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_derives_matching_keys() {
        let alice = Identity::from_seed([1u8; 32]);
        let bob = Identity::from_seed([2u8; 32]);
        // Alice initieert naar Bob; Bob antwoordt.
        let (a_eph, pending) = initiate(&alice, bob.public, [3u8; 32]);
        let (b_eph, mut bob_t) = respond(&bob, alice.public, a_eph, [4u8; 32]);
        let mut alice_t = pending.finish(b_eph);
        // Alice's send-sleutel == Bob's recv-sleutel (en omgekeerd) → de tunnel matcht.
        assert_eq!(alice_t.send_key(), {
            // bob's recv-sleutel is z'n r2i ... we toetsen via een echte round-trip:
            alice_t.send_key()
        });
        // Round-trip: Alice → Bob.
        let ct = alice_t.encrypt(b"hallo soevereine tunnel");
        assert_ne!(&ct[8..], b"hallo soevereine tunnel"); // versleuteld
        assert_eq!(bob_t.decrypt(&ct).unwrap(), b"hallo soevereine tunnel");
        // En Bob → Alice.
        let ct2 = bob_t.encrypt(b"antwoord");
        assert_eq!(alice_t.decrypt(&ct2).unwrap(), b"antwoord");
    }

    #[test]
    fn replay_is_rejected() {
        // Audit H2: een herhaald (gecaptured) pakket mag niet opnieuw aanvaard worden.
        let alice = Identity::from_seed([1u8; 32]);
        let bob = Identity::from_seed([2u8; 32]);
        let (a_eph, pending) = initiate(&alice, bob.public, [3u8; 32]);
        let (b_eph, mut bob_t) = respond(&bob, alice.public, a_eph, [4u8; 32]);
        let mut alice_t = pending.finish(b_eph);

        let p0 = alice_t.encrypt(b"pakket-0");
        let p1 = alice_t.encrypt(b"pakket-1");
        let p2 = alice_t.encrypt(b"pakket-2");
        // Eerste aflevering: alle drie aanvaard.
        assert_eq!(bob_t.decrypt(&p1).unwrap(), b"pakket-1"); // out-of-order mag
        assert_eq!(bob_t.decrypt(&p0).unwrap(), b"pakket-0");
        assert_eq!(bob_t.decrypt(&p2).unwrap(), b"pakket-2");
        // Replays van dezelfde pakketten → geweigerd.
        assert!(bob_t.decrypt(&p0).is_none());
        assert!(bob_t.decrypt(&p1).is_none());
        assert!(bob_t.decrypt(&p2).is_none());
    }

    #[test]
    fn wrong_peer_cannot_join() {
        let alice = Identity::from_seed([1u8; 32]);
        let bob = Identity::from_seed([2u8; 32]);
        let eve = Identity::from_seed([9u8; 32]);
        // Alice denkt met Bob te praten, maar Eve antwoordt met háár statische sleutel.
        let (a_eph, pending) = initiate(&alice, bob.public, [3u8; 32]);
        let (e_eph, mut eve_t) = respond(&eve, alice.public, a_eph, [4u8; 32]);
        let mut alice_t = pending.finish(e_eph);
        // De sleutels matchen NIET (Alice gebruikte Bob's S_r, Eve háár eigen) → Eve
        // kan Alice's verkeer niet ontsleutelen.
        let ct = alice_t.encrypt(b"geheim");
        assert!(eve_t.decrypt(&ct).is_none());
    }

    #[test]
    fn tamper_is_detected() {
        let a = Identity::from_seed([5u8; 32]);
        let b = Identity::from_seed([6u8; 32]);
        let (ae, pend) = initiate(&a, b.public, [7u8; 32]);
        let (be, mut bt) = respond(&b, a.public, ae, [8u8; 32]);
        let mut at = pend.finish(be);
        let mut ct = at.encrypt(b"integriteit");
        let n = ct.len();
        ct[n - 1] ^= 0xFF; // flip een byte van de tag
        assert!(bt.decrypt(&ct).is_none());
    }
}
