//! `.euroa` bundle verification (Sprint AA, step 1 — the keystone).
//!
//! An agent is distributed as an **Ed25519-signed** bundle: the manifest
//! (TOML) + the WASM binary, signed together by the publisher. The runtime must
//! **never** instantiate an agent without a valid signature against a trusted
//! public key — this way the chain "publisher → bundle → running agent" is airtight
//! and the capability isolation cannot be bypassed via a forged manifest.
//!
//! The signed message is domain-separated and length-prefixed so that manifest
//! and WASM cannot be shuffled into one another:
//! `"EuroAgent-bundle-v1\0" || len(manifest):u32-LE || manifest || wasm`.

use crate::manifest::{AgentManifest, ManifestError};
use alloc::vec::Vec;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// Domain separator — prevents a signature from another context being reused.
const DOMAIN: &[u8] = b"EuroAgent-bundle-v1\0";

/// A (not yet verified) agent bundle.
pub struct AgentBundle<'a> {
    pub manifest_toml: &'a str,
    pub wasm: &'a [u8],
    /// Ed25519 signature (64 bytes) over the domain-separated message.
    pub signature: [u8; 64],
}

/// Why a bundle was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BundleError {
    /// The public key is not a valid Ed25519 point.
    BadKey,
    /// The signature does not match for (this key, this message).
    BadSignature,
    /// The signature is correct, but the manifest is invalid.
    Manifest(ManifestError),
}

/// Build the domain-separated, length-prefixed message that gets signed.
pub fn signing_message(manifest_toml: &str, wasm: &[u8]) -> Vec<u8> {
    let mb = manifest_toml.as_bytes();
    let mut msg = Vec::with_capacity(DOMAIN.len() + 4 + mb.len() + wasm.len());
    msg.extend_from_slice(DOMAIN);
    msg.extend_from_slice(&(mb.len() as u32).to_le_bytes());
    msg.extend_from_slice(mb);
    msg.extend_from_slice(wasm);
    msg
}

impl<'a> AgentBundle<'a> {
    /// Verify the signature against `pubkey` and then parse the manifest.
    /// Returns the validated manifest **only** if the signature is correct.
    pub fn verify(&self, pubkey: &[u8; 32]) -> Result<AgentManifest, BundleError> {
        let vk = VerifyingKey::from_bytes(pubkey).map_err(|_| BundleError::BadKey)?;
        let sig = Signature::from_bytes(&self.signature);
        let msg = signing_message(self.manifest_toml, self.wasm);
        vk.verify(&msg, &sig).map_err(|_| BundleError::BadSignature)?;
        // Only after the signature do we parse the manifest (no trust before verification).
        AgentManifest::from_toml(self.manifest_toml).map_err(BundleError::Manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const MANIFEST: &str = "[agent]\nname=\"signer\"\nversion=\"1\"\nwasm=\"a.wasm\"\n[capabilities]\nrequired=[\"CAP_AGENT_FS_READ\"]\n";

    fn keypair() -> SigningKey {
        // Deterministic key for reproducible tests.
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn sign(sk: &SigningKey, manifest: &str, wasm: &[u8]) -> [u8; 64] {
        sk.sign(&signing_message(manifest, wasm)).to_bytes()
    }

    #[test]
    fn valid_bundle_verifies() {
        let sk = keypair();
        let wasm = b"\0asm\x01\0\0\0";
        let bundle = AgentBundle { manifest_toml: MANIFEST, wasm, signature: sign(&sk, MANIFEST, wasm) };
        let m = bundle.verify(&sk.verifying_key().to_bytes()).unwrap();
        assert_eq!(m.name, "signer");
    }

    #[test]
    fn tampered_manifest_rejected() {
        let sk = keypair();
        let wasm = b"\0asm";
        let sig = sign(&sk, MANIFEST, wasm);
        // Change the manifest after signing → signature must fail.
        let evil = "[agent]\nname=\"evil\"\nversion=\"1\"\nwasm=\"a.wasm\"\n[capabilities]\nrequired=[\"CAP_AGENT_EXEC\"]\n";
        let bundle = AgentBundle { manifest_toml: evil, wasm, signature: sig };
        assert_eq!(bundle.verify(&sk.verifying_key().to_bytes()), Err(BundleError::BadSignature));
    }

    #[test]
    fn tampered_wasm_rejected() {
        let sk = keypair();
        let sig = sign(&sk, MANIFEST, b"\0asm-original");
        let bundle = AgentBundle { manifest_toml: MANIFEST, wasm: b"\0asm-EVIL", signature: sig };
        assert_eq!(bundle.verify(&sk.verifying_key().to_bytes()), Err(BundleError::BadSignature));
    }

    #[test]
    fn wrong_key_rejected() {
        let sk = keypair();
        let wasm = b"w";
        let bundle = AgentBundle { manifest_toml: MANIFEST, wasm, signature: sign(&sk, MANIFEST, wasm) };
        let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key().to_bytes();
        assert_eq!(bundle.verify(&other), Err(BundleError::BadSignature));
    }
}
