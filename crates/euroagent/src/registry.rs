//! Agent-register (Sprint AA, stap 1 — tweede helft). Beheert de *geïnstalleerde*
//! agents: een agent komt er alleen in via een geldig-ondertekende bundle, wordt
//! op naam bijgehouden, en kan niet stiekem door een andere uitgever overschreven
//! worden (anti-kaping). Het register is `no_std` en serialiseerbaar zodat de
//! kernel het op EuroFS kan persisteren.

use crate::bundle::AgentBundle;
use crate::caps::AgentCaps;
use crate::manifest::AgentManifest;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Een geïnstalleerde agent: het gevalideerde manifest + wie hem ondertekende.
#[derive(Clone, Debug, PartialEq)]
pub struct InstalledAgent {
    pub name: String,
    pub version: String,
    /// De publieke sleutel (hex) van de uitgever — vastgezet bij eerste installatie.
    pub publisher: String,
    /// De effectieve caps die het manifest *vraagt* (required), als snelle index.
    pub required: AgentCaps,
    pub manifest_toml: String,
}

/// Waarom een installatie geweigerd werd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// De bundle-handtekening klopt niet (of een ongeldig manifest).
    Rejected,
    /// Er is al een agent met deze naam van een *andere* uitgever (anti-kaping).
    PublisherMismatch,
}

/// Het register van geïnstalleerde agents.
#[derive(Default)]
pub struct AgentRegistry {
    agents: Vec<InstalledAgent>,
}

fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for x in b {
        s.push_str(&alloc::format!("{x:02x}"));
    }
    s
}

impl AgentRegistry {
    pub fn new() -> Self {
        AgentRegistry { agents: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.agents.len()
    }
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    fn find_idx(&self, name: &str) -> Option<usize> {
        self.agents.iter().position(|a| a.name == name)
    }

    pub fn get(&self, name: &str) -> Option<&InstalledAgent> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// De namen van alle geïnstalleerde agents (in installatievolgorde).
    pub fn list(&self) -> Vec<&str> {
        self.agents.iter().map(|a| a.name.as_str()).collect()
    }

    /// Installeer een agent uit een bundle, geverifieerd tegen `trusted_pubkey`.
    /// Een update (zelfde naam) mag alleen van *dezelfde* uitgever. Geeft de naam terug.
    pub fn install(
        &mut self,
        bundle: &AgentBundle,
        trusted_pubkey: &[u8; 32],
    ) -> Result<String, RegistryError> {
        let manifest = bundle.verify(trusted_pubkey).map_err(|_| RegistryError::Rejected)?;
        let publisher = hex32(trusted_pubkey);

        if let Some(idx) = self.find_idx(&manifest.name) {
            if self.agents[idx].publisher != publisher {
                return Err(RegistryError::PublisherMismatch);
            }
        }

        let entry = InstalledAgent {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            publisher,
            required: manifest.required,
            manifest_toml: bundle.manifest_toml.to_string(),
        };
        let name = entry.name.clone();
        match self.find_idx(&name) {
            Some(idx) => self.agents[idx] = entry, // update
            None => self.agents.push(entry),
        }
        Ok(name)
    }

    /// Verwijder een agent. `true` als hij bestond.
    pub fn remove(&mut self, name: &str) -> bool {
        match self.find_idx(name) {
            Some(idx) => {
                self.agents.remove(idx);
                true
            }
            None => false,
        }
    }

    /// Serialiseer de index naar een regelgebaseerd, persisteerbaar formaat
    /// (`name\tversion\tpublisher\trequired_bits`). Het manifest zelf wordt
    /// content-adres-gewijs apart opgeslagen door de kernel.
    pub fn serialize_index(&self) -> Vec<u8> {
        let mut s = String::from("# euroagent-registry v1\n");
        for a in &self.agents {
            s.push_str(&alloc::format!("{}\t{}\t{}\t{}\n", a.name, a.version, a.publisher, a.required.0));
        }
        s.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::signing_message;
    use ed25519_dalek::{Signer, SigningKey};

    fn manifest(name: &str) -> String {
        alloc::format!(
            "[agent]\nname=\"{name}\"\nversion=\"1.0\"\nwasm=\"a.wasm\"\n[capabilities]\nrequired=[\"CAP_AGENT_FS_READ\"]\n"
        )
    }

    fn signed<'a>(sk: &SigningKey, m: &'a str, wasm: &'a [u8]) -> AgentBundle<'a> {
        let sig = sk.sign(&signing_message(m, wasm)).to_bytes();
        AgentBundle { manifest_toml: m, wasm, signature: sig }
    }

    #[test]
    fn install_and_list() {
        let sk = SigningKey::from_bytes(&[1u8; 32]);
        let pk = sk.verifying_key().to_bytes();
        let mut reg = AgentRegistry::new();
        let m = manifest("alpha");
        let name = reg.install(&signed(&sk, &m, b"w"), &pk).unwrap();
        assert_eq!(name, "alpha");
        assert_eq!(reg.list(), alloc::vec!["alpha"]);
        assert_eq!(reg.get("alpha").unwrap().version, "1.0");
    }

    #[test]
    fn bad_signature_rejected() {
        let sk = SigningKey::from_bytes(&[1u8; 32]);
        let other = SigningKey::from_bytes(&[2u8; 32]).verifying_key().to_bytes();
        let mut reg = AgentRegistry::new();
        let m = manifest("alpha");
        assert_eq!(reg.install(&signed(&sk, &m, b"w"), &other), Err(RegistryError::Rejected));
        assert!(reg.is_empty());
    }

    #[test]
    fn publisher_hijack_blocked() {
        let sk1 = SigningKey::from_bytes(&[1u8; 32]);
        let sk2 = SigningKey::from_bytes(&[2u8; 32]);
        let mut reg = AgentRegistry::new();
        let m = manifest("alpha");
        reg.install(&signed(&sk1, &m, b"w"), &sk1.verifying_key().to_bytes()).unwrap();
        // Een andere uitgever mag 'alpha' niet overschrijven, ook al is zijn eigen
        // handtekening geldig.
        let r = reg.install(&signed(&sk2, &m, b"w"), &sk2.verifying_key().to_bytes());
        assert_eq!(r, Err(RegistryError::PublisherMismatch));
        // De originele uitgever mag wél updaten.
        let m2 = manifest("alpha").replace("1.0", "2.0");
        reg.install(&signed(&sk1, &m2, b"w"), &sk1.verifying_key().to_bytes()).unwrap();
        assert_eq!(reg.get("alpha").unwrap().version, "2.0");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn remove_works() {
        let sk = SigningKey::from_bytes(&[3u8; 32]);
        let pk = sk.verifying_key().to_bytes();
        let mut reg = AgentRegistry::new();
        reg.install(&signed(&sk, &manifest("a"), b"w"), &pk).unwrap();
        reg.install(&signed(&sk, &manifest("b"), b"w"), &pk).unwrap();
        assert!(reg.remove("a"));
        assert!(!reg.remove("a"));
        assert_eq!(reg.list(), alloc::vec!["b"]);
        // De index serialiseert.
        let idx = reg.serialize_index();
        assert!(String::from_utf8(idx).unwrap().contains("b\t1.0"));
    }
}
