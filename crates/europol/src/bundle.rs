//! 3D-4 — **signed policy bundles**.
//!
//! A capability policy is a security-critical input: if an attacker can swap it,
//! they can grant themselves capabilities. So a policy bundle is serialized to a
//! canonical byte form, **signed with Ed25519**, and **verify-before-load**: an
//! unsigned or tampered bundle is refused and the system keeps its safe default
//! (no elevation). Same trust model as the A/B update + verity signatures.

use crate::Policy;
use alloc::string::String;
use alloc::vec::Vec;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

const MAGIC: &[u8] = b"EuroPol-bundle-v1\0";

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}
fn put_strs(out: &mut Vec<u8>, v: &[String]) {
    out.extend_from_slice(&(v.len() as u32).to_le_bytes());
    for s in v {
        put_str(out, s);
    }
}

/// Canonical byte encoding of a set of policies (deterministic → stable
/// signatures).
pub fn serialize(policies: &[Policy]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(policies.len() as u32).to_le_bytes());
    for p in policies {
        put_str(&mut out, &p.name);
        out.extend_from_slice(&p.allow_caps.to_le_bytes());
        out.extend_from_slice(&p.deny_caps.to_le_bytes());
        put_strs(&mut out, &p.allow_paths);
        put_strs(&mut out, &p.deny_paths);
        out.push(p.log_denied as u8);
    }
    out
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}
impl Reader<'_> {
    fn u32(&mut self) -> Option<u32> {
        let s = self.b.get(self.p..self.p + 4)?;
        self.p += 4;
        Some(u32::from_le_bytes(s.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        let s = self.b.get(self.p..self.p + 8)?;
        self.p += 8;
        Some(u64::from_le_bytes(s.try_into().ok()?))
    }
    fn byte(&mut self) -> Option<u8> {
        let v = *self.b.get(self.p)?;
        self.p += 1;
        Some(v)
    }
    fn string(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        let b = self.b.get(self.p..self.p + n)?;
        self.p += n;
        Some(String::from_utf8_lossy(b).into_owned())
    }
    fn strings(&mut self) -> Option<Vec<String>> {
        let n = self.u32()? as usize;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.string()?);
        }
        Some(v)
    }
}

/// Parse a bundle's canonical bytes back into policies (bounds-checked).
pub fn deserialize(bytes: &[u8]) -> Option<Vec<Policy>> {
    if !bytes.starts_with(MAGIC) {
        return None;
    }
    let mut r = Reader { b: bytes, p: MAGIC.len() };
    let n = r.u32()? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let name = r.string()?;
        let allow_caps = r.u64()?;
        let deny_caps = r.u64()?;
        let allow_paths = r.strings()?;
        let deny_paths = r.strings()?;
        let log_denied = r.byte()? != 0;
        out.push(Policy { name, allow_caps, deny_caps, allow_paths, deny_paths, log_denied });
    }
    Some(out)
}

/// Sign a serialized bundle with the release key.
pub fn sign(bytes: &[u8], key: &SigningKey) -> [u8; 64] {
    key.sign(bytes).to_bytes()
}

/// Verify a bundle signature against the release public key.
pub fn verify(bytes: &[u8], pubkey: &[u8; 32], sig: &[u8; 64]) -> bool {
    match VerifyingKey::from_bytes(pubkey) {
        Ok(vk) => vk.verify(bytes, &Signature::from_bytes(sig)).is_ok(),
        Err(_) => false,
    }
}

/// **Verify-before-load**: return the policies only if the signature is valid.
/// A tampered or unsigned bundle yields `None` — the caller keeps its default.
pub fn load_verified(bytes: &[u8], sig: &[u8; 64], pubkey: &[u8; 32]) -> Option<Vec<Policy>> {
    if verify(bytes, pubkey, sig) {
        deserialize(bytes)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Policy> {
        alloc::vec![
            crate::parse("name = \"browser\"\n[allow]\ncapabilities = [\"CAP_NET\", \"CAP_CONSOLE\"]\npaths = [\"/home\"]\n[deny]\npaths = [\"/etc\"]\nlog_denied = true"),
            crate::parse("name = \"agent\"\n[allow]\ncapabilities = [\"CAP_FILE\"]"),
        ]
    }

    #[test]
    fn serialize_roundtrip() {
        let ps = sample();
        let bytes = serialize(&ps);
        assert_eq!(deserialize(&bytes).unwrap(), ps);
    }

    #[test]
    fn sign_and_verify_load() {
        let ps = sample();
        let bytes = serialize(&ps);
        let key = SigningKey::from_bytes(&[0x33; 32]);
        let pk = key.verifying_key().to_bytes();
        let sig = sign(&bytes, &key);
        assert_eq!(load_verified(&bytes, &sig, &pk), Some(ps));
    }

    #[test]
    fn tampered_bundle_refused() {
        let bytes = serialize(&sample());
        let key = SigningKey::from_bytes(&[0x33; 32]);
        let pk = key.verifying_key().to_bytes();
        let sig = sign(&bytes, &key);
        let mut evil = bytes.clone();
        // Flip a capability bit in the serialized allow_caps of the first policy.
        let off = MAGIC.len() + 4 + 4 + "browser".len();
        evil[off] ^= 0x01;
        assert_eq!(load_verified(&evil, &sig, &pk), None);
    }

    #[test]
    fn wrong_key_refused() {
        let bytes = serialize(&sample());
        let key = SigningKey::from_bytes(&[0x33; 32]);
        let sig = sign(&bytes, &key);
        let other = SigningKey::from_bytes(&[0x44; 32]).verifying_key().to_bytes();
        assert_eq!(load_verified(&bytes, &sig, &other), None);
    }
}
