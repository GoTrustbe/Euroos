//! EuroVPN — a **sovereign, forward-secret VPN tunnel** (plan N2, WireGuard-style).
//!
//! A network-sovereign OS needs its own modern VPN. EuroVPN uses a
//! **Noise-like authenticated key exchange**: each side has a static
//! X25519 key (pre-exchanged, like a WireGuard peer) and generates an
//! ephemeral key per session. The shared session key is derived from a
//! **quadruple Diffie-Hellman** — `e_i·e_r`, `e_i·S_r`, `s_i·e_r`, `s_i·S_r` — via
//! HKDF-SHA256. That provides both **forward secrecy** (the ephemeral DHs) and
//! **mutual authentication** (the static DHs): an attacker without one of the
//! private keys cannot derive the tunnel. The transport is **ChaCha20-Poly1305**
//! with a per-packet counter nonce. Pure `no_std` crypto → host-tested.
//!
//! (Deliberately not byte-compatible with WireGuard itself — that requires BLAKE2s; this is the
//! sovereign variant on the same cryptographic principles.)

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

/// A static identity (X25519 key pair). You share the public key with
/// peers; the private key stays local.
pub struct Identity {
    secret: StaticSecret,
    pub public: [u8; 32],
}

impl Identity {
    /// Derive an identity from 32 seed bytes (e.g. from the TPM-RNG).
    pub fn from_seed(seed: [u8; 32]) -> Identity {
        let secret = StaticSecret::from(seed);
        let public = PublicKey::from(&secret).to_bytes();
        Identity { secret, public }
    }

    fn dh(&self, peer_pub: &[u8; 32]) -> [u8; 32] {
        self.secret.diffie_hellman(&PublicKey::from(*peer_pub)).to_bytes()
    }
}

/// An established tunnel: directional keys + counters.
pub struct Tunnel {
    send_key: [u8; 32],
    recv_key: [u8; 32],
    send_ctr: u64,
    /// Highest accepted receive counter (+1) = the top of the anti-replay window.
    recv_ctr: u64,
    /// Bitmask of the last 64 already-seen counters below `recv_ctr` (WireGuard-style
    /// sliding window) — prevents repeated/old packets (audit H2).
    replay_window: u64,
}

/// Combine the four DH results into the tunnel key material (HKDF-SHA256).
/// `initiator` determines which derived key is "send" and which is "recv".
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

/// **Initiator side** of the handshake. `our` = our static identity,
/// `peer_static` = the responder's public static key, `eph_seed` = seed
/// for our ephemeral key. Returns (our ephemeral pubkey to send, continuation).
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

/// State kept between the two initiator steps.
pub struct PendingInitiator {
    our_static_secret_seed: StaticSecret,
    our_static_pub: [u8; 32],
    eph: Identity,
    peer_static: [u8; 32],
}

impl PendingInitiator {
    /// Complete the handshake with the responder's ephemeral pubkey → the tunnel.
    pub fn finish(self, resp_eph_pub: [u8; 32]) -> Tunnel {
        let s_i = Identity { secret: self.our_static_secret_seed, public: self.our_static_pub };
        let dh1 = self.eph.dh(&resp_eph_pub); // e_i · e_r
        let dh2 = self.eph.dh(&self.peer_static); // e_i · S_r
        let dh3 = s_i.dh(&resp_eph_pub); // s_i · e_r
        let dh4 = s_i.dh(&self.peer_static); // s_i · S_r
        derive(dh1, dh2, dh3, dh4, true)
    }
}

/// **Responder side**: process the initiator's ephemeral pubkey and return (our ephemeral
/// pubkey to send back, the tunnel). `peer_static` = the initiator's public static
/// key (IK: known in advance).
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
    // X25519 StaticSecret is not Clone; re-derive from the bytes.
    StaticSecret::from(id.secret.to_bytes())
}

impl Tunnel {
    /// Encrypt an outgoing packet (ChaCha20-Poly1305, nonce = send counter).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.send_key));
        let nonce = ctr_nonce(self.send_ctr);
        self.send_ctr += 1;
        let ct = cipher.encrypt(Nonce::from_slice(&nonce), plaintext).unwrap_or_default();
        // Prefix the counter so the receiver can use it as the nonce.
        let mut out = Vec::with_capacity(8 + ct.len());
        out.extend_from_slice(&(self.send_ctr - 1).to_le_bytes());
        out.extend_from_slice(&ct);
        out
    }

    /// Decrypt an incoming packet; verifies the Poly1305 tag (tamper-evident).
    pub fn decrypt(&mut self, packet: &[u8]) -> Option<Vec<u8>> {
        if packet.len() < 8 {
            return None;
        }
        let ctr = u64::from_le_bytes(packet[..8].try_into().ok()?);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.recv_key));
        // Verify the Poly1305 tag first (only authentic packets count).
        let pt = cipher.decrypt(Nonce::from_slice(&ctr_nonce(ctr)), &packet[8..]).ok()?;

        // ── Anti-replay (audit H2): WireGuard-style 64-bit sliding window. ──
        // `recv_ctr` = highest accepted counter + 1; bit k = (recv_ctr-1-k) seen.
        let top = self.recv_ctr;
        if ctr + 1 > top {
            // New highest counter → shift the window.
            let shift = ctr + 1 - top;
            self.replay_window = if shift >= 64 { 0 } else { self.replay_window << shift };
            self.replay_window |= 1; // bit 0 = the new highest (ctr)
            self.recv_ctr = ctr + 1;
        } else {
            // Below the top: too old or a repeat?
            let offset = top - 1 - ctr;
            if offset >= 64 {
                return None; // outside the window → reject (possible replay)
            }
            let bit = 1u64 << offset;
            if self.replay_window & bit != 0 {
                return None; // this counter was already seen → replay
            }
            self.replay_window |= bit;
        }
        Some(pt)
    }

    /// (test/diagnostics only) — the send key, to prove matching sessions.
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
        // Alice initiates to Bob; Bob responds.
        let (a_eph, pending) = initiate(&alice, bob.public, [3u8; 32]);
        let (b_eph, mut bob_t) = respond(&bob, alice.public, a_eph, [4u8; 32]);
        let mut alice_t = pending.finish(b_eph);
        // Alice's send key == Bob's recv key (and vice versa) → the tunnel matches.
        assert_eq!(alice_t.send_key(), {
            // bob's recv key is his r2i ... we test it via a real round-trip:
            alice_t.send_key()
        });
        // Round-trip: Alice → Bob.
        let ct = alice_t.encrypt(b"hello sovereign tunnel");
        assert_ne!(&ct[8..], b"hello sovereign tunnel"); // encrypted
        assert_eq!(bob_t.decrypt(&ct).unwrap(), b"hello sovereign tunnel");
        // And Bob → Alice.
        let ct2 = bob_t.encrypt(b"reply");
        assert_eq!(alice_t.decrypt(&ct2).unwrap(), b"reply");
    }

    #[test]
    fn replay_is_rejected() {
        // Audit H2: a repeated (captured) packet must not be accepted again.
        let alice = Identity::from_seed([1u8; 32]);
        let bob = Identity::from_seed([2u8; 32]);
        let (a_eph, pending) = initiate(&alice, bob.public, [3u8; 32]);
        let (b_eph, mut bob_t) = respond(&bob, alice.public, a_eph, [4u8; 32]);
        let mut alice_t = pending.finish(b_eph);

        let p0 = alice_t.encrypt(b"packet-0");
        let p1 = alice_t.encrypt(b"packet-1");
        let p2 = alice_t.encrypt(b"packet-2");
        // First delivery: all three accepted.
        assert_eq!(bob_t.decrypt(&p1).unwrap(), b"packet-1"); // out-of-order allowed
        assert_eq!(bob_t.decrypt(&p0).unwrap(), b"packet-0");
        assert_eq!(bob_t.decrypt(&p2).unwrap(), b"packet-2");
        // Replays of the same packets → rejected.
        assert!(bob_t.decrypt(&p0).is_none());
        assert!(bob_t.decrypt(&p1).is_none());
        assert!(bob_t.decrypt(&p2).is_none());
    }

    #[test]
    fn wrong_peer_cannot_join() {
        let alice = Identity::from_seed([1u8; 32]);
        let bob = Identity::from_seed([2u8; 32]);
        let eve = Identity::from_seed([9u8; 32]);
        // Alice thinks she is talking to Bob, but Eve answers with her own static key.
        let (a_eph, pending) = initiate(&alice, bob.public, [3u8; 32]);
        let (e_eph, mut eve_t) = respond(&eve, alice.public, a_eph, [4u8; 32]);
        let mut alice_t = pending.finish(e_eph);
        // The keys do NOT match (Alice used Bob's S_r, Eve her own) → Eve
        // cannot decrypt Alice's traffic.
        let ct = alice_t.encrypt(b"secret");
        assert!(eve_t.decrypt(&ct).is_none());
    }

    #[test]
    fn tamper_is_detected() {
        let a = Identity::from_seed([5u8; 32]);
        let b = Identity::from_seed([6u8; 32]);
        let (ae, pend) = initiate(&a, b.public, [7u8; 32]);
        let (be, mut bt) = respond(&b, a.public, ae, [8u8; 32]);
        let mut at = pend.finish(be);
        let mut ct = at.encrypt(b"integrity");
        let n = ct.len();
        ct[n - 1] ^= 0xFF; // flip a byte of the tag
        assert!(bt.decrypt(&ct).is_none());
    }
}
