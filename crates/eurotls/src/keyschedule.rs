//! TLS 1.3 sleutelschema (RFC 8446 §7.1) met SHA-256: HKDF-Extract,
//! HKDF-Expand-Label, Derive-Secret en de afleiding van de traffic-secrets.

use alloc::vec;
use alloc::vec::Vec;

use hkdf::Hkdf;
use sha2::{Digest, Sha256};

pub const HASH_LEN: usize = 32; // SHA-256
pub const KEY_LEN: usize = 32; // ChaCha20 sleutel
pub const IV_LEN: usize = 12; // AEAD nonce-basis

/// HKDF-Extract(salt, IKM) -> PRK (32 bytes).
pub fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; HASH_LEN] {
    let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), ikm);
    let mut out = [0u8; HASH_LEN];
    out.copy_from_slice(&prk);
    out
}

/// HKDF-Expand-Label(secret, label, context, length) (RFC 8446 §7.1). De label
/// krijgt het verplichte "tls13 "-voorvoegsel.
pub fn hkdf_expand_label(secret: &[u8], label: &str, context: &[u8], length: usize) -> Vec<u8> {
    // struct HkdfLabel { uint16 length; opaque label<7..255>; opaque context<0..255>; }
    let mut full_label = Vec::with_capacity(6 + label.len());
    full_label.extend_from_slice(b"tls13 ");
    full_label.extend_from_slice(label.as_bytes());

    let mut info = Vec::with_capacity(2 + 1 + full_label.len() + 1 + context.len());
    info.extend_from_slice(&(length as u16).to_be_bytes());
    info.push(full_label.len() as u8);
    info.extend_from_slice(&full_label);
    info.push(context.len() as u8);
    info.extend_from_slice(context);

    let hk = Hkdf::<Sha256>::from_prk(secret).expect("prk lengte >= 32");
    let mut out = vec![0u8; length];
    hk.expand(&info, &mut out).expect("hkdf expand lengte ok");
    out
}

/// Derive-Secret(secret, label, transcript-hash) -> 32 bytes.
pub fn derive_secret(secret: &[u8], label: &str, transcript_hash: &[u8]) -> [u8; HASH_LEN] {
    let v = hkdf_expand_label(secret, label, transcript_hash, HASH_LEN);
    let mut out = [0u8; HASH_LEN];
    out.copy_from_slice(&v);
    out
}

/// SHA-256 over een set bytes (de "lege" transcript-hash = SHA-256("")).
pub fn sha256(data: &[u8]) -> [u8; HASH_LEN] {
    let mut h = Sha256::new();
    h.update(data);
    let d = h.finalize();
    let mut out = [0u8; HASH_LEN];
    out.copy_from_slice(&d);
    out
}

/// Lopende transcript-hash van handshake-berichten (RFC 8446 §4.4.1).
#[derive(Clone)]
pub struct Transcript {
    hasher: Sha256,
}

impl Transcript {
    pub fn new() -> Self {
        Transcript { hasher: Sha256::new() }
    }
    /// Voeg een handshake-bericht (incl. 4-byte handshake-header) toe.
    pub fn update(&mut self, msg: &[u8]) {
        self.hasher.update(msg);
    }
    /// Huidige transcript-hash (kloont, finaliseert niet de echte staat).
    pub fn hash(&self) -> [u8; HASH_LEN] {
        let d = self.hasher.clone().finalize();
        let mut out = [0u8; HASH_LEN];
        out.copy_from_slice(&d);
        out
    }
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

/// Een (key, iv) paar plus de finished-sleutel, afgeleid uit een traffic-secret.
pub struct TrafficKeys {
    pub secret: [u8; HASH_LEN],
    pub key: [u8; KEY_LEN],
    pub iv: [u8; IV_LEN],
    pub finished_key: [u8; HASH_LEN],
}

impl TrafficKeys {
    pub fn derive(secret: [u8; HASH_LEN]) -> Self {
        let key_v = hkdf_expand_label(&secret, "key", b"", KEY_LEN);
        let iv_v = hkdf_expand_label(&secret, "iv", b"", IV_LEN);
        let fin_v = hkdf_expand_label(&secret, "finished", b"", HASH_LEN);
        let mut key = [0u8; KEY_LEN];
        let mut iv = [0u8; IV_LEN];
        let mut finished_key = [0u8; HASH_LEN];
        key.copy_from_slice(&key_v);
        iv.copy_from_slice(&iv_v);
        finished_key.copy_from_slice(&fin_v);
        TrafficKeys { secret, key, iv, finished_key }
    }
}

/// Het volledige sleutelschema, stap voor stap opgebouwd tijdens de handshake.
pub struct KeySchedule {
    pub early_secret: [u8; HASH_LEN],
    pub handshake_secret: [u8; HASH_LEN],
    pub master_secret: [u8; HASH_LEN],
}

impl KeySchedule {
    /// Begin: Early Secret = HKDF-Extract(0, 0).
    pub fn new() -> Self {
        let early_secret = hkdf_extract(&[0u8; HASH_LEN], &[0u8; HASH_LEN]);
        KeySchedule { early_secret, handshake_secret: [0; HASH_LEN], master_secret: [0; HASH_LEN] }
    }

    /// Na de ECDHE: Handshake Secret = HKDF-Extract(Derive-Secret(ES,"derived",""), ECDHE).
    /// Geeft de (client, server) handshake-traffic-secrets terug, afgeleid over
    /// de transcript t/m ServerHello.
    pub fn derive_handshake(&mut self, ecdhe: &[u8; 32], th_client_server_hello: &[u8; HASH_LEN]) -> (TrafficKeys, TrafficKeys) {
        let derived = derive_secret(&self.early_secret, "derived", &sha256(b""));
        self.handshake_secret = hkdf_extract(&derived, ecdhe);
        let cs = derive_secret(&self.handshake_secret, "c hs traffic", th_client_server_hello);
        let ss = derive_secret(&self.handshake_secret, "s hs traffic", th_client_server_hello);
        (TrafficKeys::derive(cs), TrafficKeys::derive(ss))
    }

    /// Master Secret + de (client, server) application-traffic-secrets, afgeleid
    /// over de transcript t/m de server-Finished.
    pub fn derive_application(&mut self, th_to_server_finished: &[u8; HASH_LEN]) -> (TrafficKeys, TrafficKeys) {
        let derived = derive_secret(&self.handshake_secret, "derived", &sha256(b""));
        self.master_secret = hkdf_extract(&derived, &[0u8; HASH_LEN]);
        let cs = derive_secret(&self.master_secret, "c ap traffic", th_to_server_finished);
        let ss = derive_secret(&self.master_secret, "s ap traffic", th_to_server_finished);
        (TrafficKeys::derive(cs), TrafficKeys::derive(ss))
    }
}

impl Default for KeySchedule {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_transcript_hash_matches_sha256_empty() {
        // SHA-256("") = e3b0c442...
        let t = Transcript::new();
        assert_eq!(
            t.hash(),
            sha256(b"")
        );
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn expand_label_structure() {
        // HKDF-Expand-Label moet deterministisch en lengte-correct zijn.
        let secret = [0x42u8; 32];
        let out = hkdf_expand_label(&secret, "key", b"", 32);
        assert_eq!(out.len(), 32);
        // Tweede keer identiek (deterministisch).
        assert_eq!(out, hkdf_expand_label(&secret, "key", b"", 32));
        // Andere label -> ander resultaat.
        assert_ne!(out, hkdf_expand_label(&secret, "iv", b"", 32));
    }

    #[test]
    fn rfc8446_derived_secret_constant() {
        // Derive-Secret(Early Secret, "derived", "") is een vaste waarde omdat
        // Early Secret = HKDF-Extract(0,0) constant is. Dit pint het sleutelschema
        // vast tegen regressies in HKDF-Expand-Label/Derive-Secret.
        let ks = KeySchedule::new();
        let derived = derive_secret(&ks.early_secret, "derived", &sha256(b""));
        assert_eq!(
            hex(&ks.early_secret),
            "33ad0a1c607ec03b09e6cd9893680ce210adf300aa1f2660e1b22e10f170f92a"
        );
        assert_eq!(
            hex(&derived),
            "6f2615a108c702c5678f54fc9dbab69716c076189c48250cebeac3576c3611ba"
        );
    }

    fn hex(b: &[u8]) -> alloc::string::String {
        use alloc::string::String;
        let mut s = String::new();
        for x in b {
            s.push_str(&alloc::format!("{x:02x}"));
        }
        s
    }
}
