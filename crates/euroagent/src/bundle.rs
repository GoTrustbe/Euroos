//! `.euroa`-bundle-verificatie (Sprint AA, stap 1 — sluitstuk).
//!
//! Een agent wordt verspreid als een **Ed25519-gesigneerde** bundle: het manifest
//! (TOML) + de WASM-binary, samen ondertekend door de uitgever. De runtime mag een
//! agent **nooit** instantiëren zonder een geldige handtekening tegen een vertrouwde
//! publieke sleutel — zo is de keten "uitgever → bundle → draaiende agent" sluitend
//! en is de capability-isolatie niet te omzeilen via een vervalst manifest.
//!
//! Het ondertekende bericht is domein-gescheiden en lengte-geprefixt zodat manifest
//! en WASM niet door elkaar te schuiven zijn:
//! `"EuroAgent-bundle-v1\0" || len(manifest):u32-LE || manifest || wasm`.

use crate::manifest::{AgentManifest, ManifestError};
use alloc::vec::Vec;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// Domeinscheider — voorkomt dat een handtekening uit een andere context hergebruikt wordt.
const DOMAIN: &[u8] = b"EuroAgent-bundle-v1\0";

/// Een (nog niet geverifieerde) agent-bundle.
pub struct AgentBundle<'a> {
    pub manifest_toml: &'a str,
    pub wasm: &'a [u8],
    /// Ed25519-handtekening (64 bytes) over het domein-gescheiden bericht.
    pub signature: [u8; 64],
}

/// Waarom een bundle afgewezen werd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BundleError {
    /// De publieke sleutel is geen geldig Ed25519-punt.
    BadKey,
    /// De handtekening klopt niet voor (deze sleutel, dit bericht).
    BadSignature,
    /// De handtekening klopt, maar het manifest is ongeldig.
    Manifest(ManifestError),
}

/// Bouw het domein-gescheiden, lengte-geprefixte bericht dat ondertekend wordt.
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
    /// Verifieer de handtekening tegen `pubkey` en parse dan het manifest.
    /// Geeft het gevalideerde manifest **alleen** terug als de handtekening klopt.
    pub fn verify(&self, pubkey: &[u8; 32]) -> Result<AgentManifest, BundleError> {
        let vk = VerifyingKey::from_bytes(pubkey).map_err(|_| BundleError::BadKey)?;
        let sig = Signature::from_bytes(&self.signature);
        let msg = signing_message(self.manifest_toml, self.wasm);
        vk.verify(&msg, &sig).map_err(|_| BundleError::BadSignature)?;
        // Pas ná de handtekening parsen we het manifest (geen vertrouwen vóór verificatie).
        AgentManifest::from_toml(self.manifest_toml).map_err(BundleError::Manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const MANIFEST: &str = "[agent]\nname=\"signer\"\nversion=\"1\"\nwasm=\"a.wasm\"\n[capabilities]\nrequired=[\"CAP_AGENT_FS_READ\"]\n";

    fn keypair() -> SigningKey {
        // Deterministische sleutel voor reproduceerbare tests.
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
        // Verander het manifest ná het tekenen → handtekening moet falen.
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
