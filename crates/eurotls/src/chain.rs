//! X.509-ketenvalidatie (plan A1, fase 3): bind een door de server aangeboden
//! certificaatketen aan een vertrouwde root, met geldigheids- en hostnaamcontrole.
//! Een vereenvoudigde RFC 5280 §6.1-padvalidatie: naam-koppeling + handtekening
//! per stap + basicConstraints(CA) op tussencertificaten + tijdvenster + SAN.
//!
//! Geen paniek op rommel; elke afwijking geeft een `ChainError`.

use alloc::vec::Vec;

use crate::sig;
use crate::x509::{Certificate, PubKeyAlg, X509Error};

/// Reden waarom een keten niet vertrouwd kon worden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainError {
    /// De keten was leeg.
    EmptyChain,
    /// Een certificaat kon niet ontleed worden.
    Parse(X509Error),
    /// Een certificaat is verlopen of nog niet geldig op het peilmoment.
    Expired,
    /// De hostnaam matcht met geen enkele SubjectAltName-dNSName.
    HostnameMismatch,
    /// issuer (kind) ≠ subject (uitgever): de keten is niet aaneengesloten.
    BrokenChain,
    /// Een uitgever in de keten is geen CA (basicConstraints CA:TRUE ontbreekt).
    IssuerNotCa,
    /// Een handtekening in de keten klopte niet.
    BadSignature,
    /// Geen van de ketenankerpunten zit in de trust store.
    UnknownCa,
}

impl From<X509Error> for ChainError {
    fn from(e: X509Error) -> Self {
        ChainError::Parse(e)
    }
}

/// Een verzameling vertrouwde root-CA's. Leent de DER-bytes (`'a`), zodat de
/// kernel een `'static` gebundelde EU/Mozilla-store kan aanbieden zonder kopie.
pub struct TrustStore<'a> {
    roots: Vec<Certificate<'a>>,
}

impl<'a> TrustStore<'a> {
    /// Bouw een trust store uit DER-gecodeerde root-certificaten. Onleesbare
    /// roots worden overgeslagen (een kapotte bundel-entry sloopt de store niet).
    pub fn from_ders(ders: &[&'a [u8]]) -> Self {
        let mut roots = Vec::new();
        for der in ders {
            if let Ok(c) = Certificate::parse(der) {
                roots.push(c);
            }
        }
        TrustStore { roots }
    }

    pub fn len(&self) -> usize {
        self.roots.len()
    }
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Zoek een vertrouwde root waarvan de subject-naam gelijk is aan `issuer`.
    fn find_issuer(&self, issuer: &[u8]) -> Option<&Certificate<'a>> {
        self.roots.iter().find(|r| r.subject_der == issuer)
    }
}

/// Validatieresultaat: het geverifieerde leaf-sleutelmateriaal, klaar om de
/// server-handtekening in de TLS-handshake (CertificateVerify) mee te checken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub leaf_pubkey_alg: PubKeyAlg,
    pub leaf_pubkey: Vec<u8>,
}

/// Valideer een certificaatketen (leaf eerst, daarna tussencertificaten; de
/// root hoort in de trust store, niet in de keten). `now` = epoch-seconden.
pub fn validate(chain_der: &[&[u8]], hostname: &str, now: i64, trust: &TrustStore) -> Result<Verified, ChainError> {
    if chain_der.is_empty() {
        return Err(ChainError::EmptyChain);
    }
    // Ontleed de hele keten.
    let mut certs: Vec<Certificate> = Vec::with_capacity(chain_der.len());
    for der in chain_der {
        certs.push(Certificate::parse(der)?);
    }

    // Leaf: tijdvenster + hostnaam.
    let leaf = &certs[0];
    if !leaf.valid_at(now) {
        return Err(ChainError::Expired);
    }
    if !leaf.matches_hostname(hostname) {
        return Err(ChainError::HostnameMismatch);
    }

    // Loop door de keten en probeer bij elk certificaat te verankeren zodra zijn
    // uitgever een vertrouwde root is. Zo kunnen extra cross-sign-certificaten die
    // de server bovenop een al vertrouwde tussenroot stuurt, genegeerd worden
    // (bv. ISRG Root X2 cross-signed door X1, terwijl X2 zelf al vertrouwd is).
    for i in 0..certs.len() {
        let cert = &certs[i];
        // Anker: is de uitgever van dit certificaat een vertrouwde root?
        if let Some(root) = trust.find_issuer(cert.issuer_der) {
            if root.valid_at(now)
                && sig::verify(cert.sig_alg, root.pubkey_alg, root.pubkey, cert.tbs_der, cert.signature)
            {
                return Ok(Verified {
                    leaf_pubkey_alg: leaf.pubkey_alg,
                    leaf_pubkey: leaf.pubkey.to_vec(),
                });
            }
        }
        // Anders: dit certificaat moet door het volgende in de keten ondertekend
        // zijn, en dat volgende moet een geldige CA zijn.
        if i + 1 >= certs.len() {
            return Err(ChainError::UnknownCa); // geen anker gevonden
        }
        let issuer = &certs[i + 1];
        if cert.issuer_der != issuer.subject_der {
            return Err(ChainError::BrokenChain);
        }
        if !issuer.is_ca {
            return Err(ChainError::IssuerNotCa);
        }
        if !issuer.valid_at(now) {
            return Err(ChainError::Expired);
        }
        if !sig::verify(cert.sig_alg, issuer.pubkey_alg, issuer.pubkey, cert.tbs_der, cert.signature) {
            return Err(ChainError::BadSignature);
        }
    }
    Err(ChainError::UnknownCa)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EC_ROOT: &[u8] = include_bytes!("../testdata/ec_root.der");
    const EC_LEAF: &[u8] = include_bytes!("../testdata/ec_leaf.der");
    const RSA: &[u8] = include_bytes!("../testdata/rsa.der");

    // Een peilmoment dat zeker binnen het geldigheidsvenster van de leaf valt.
    fn mid_validity() -> i64 {
        let leaf = Certificate::parse(EC_LEAF).unwrap();
        leaf.not_before + 100
    }

    #[test]
    fn valid_ec_chain_anchored_to_root() {
        let trust = TrustStore::from_ders(&[EC_ROOT]);
        let v = validate(&[EC_LEAF], "example.test", mid_validity(), &trust).unwrap();
        assert_eq!(v.leaf_pubkey_alg, PubKeyAlg::EcP256);
        assert_eq!(v.leaf_pubkey.len(), 65);
        // Ook de tweede SAN matcht.
        assert!(validate(&[EC_LEAF], "www.example.test", mid_validity(), &trust).is_ok());
    }

    #[test]
    fn wrong_hostname_rejected() {
        let trust = TrustStore::from_ders(&[EC_ROOT]);
        assert_eq!(
            validate(&[EC_LEAF], "evil.test", mid_validity(), &trust),
            Err(ChainError::HostnameMismatch)
        );
    }

    #[test]
    fn expired_and_not_yet_valid_rejected() {
        let trust = TrustStore::from_ders(&[EC_ROOT]);
        let leaf = Certificate::parse(EC_LEAF).unwrap();
        assert_eq!(validate(&[EC_LEAF], "example.test", leaf.not_after + 1, &trust), Err(ChainError::Expired));
        assert_eq!(validate(&[EC_LEAF], "example.test", leaf.not_before - 1, &trust), Err(ChainError::Expired));
    }

    #[test]
    fn empty_trust_store_is_unknown_ca() {
        let trust = TrustStore::from_ders(&[]);
        assert_eq!(validate(&[EC_LEAF], "example.test", mid_validity(), &trust), Err(ChainError::UnknownCa));
    }

    #[test]
    fn wrong_root_is_unknown_ca() {
        // De leaf zelf als "root" → zijn subject ≠ de issuer-naam van de leaf.
        let trust = TrustStore::from_ders(&[EC_LEAF]);
        assert_eq!(validate(&[EC_LEAF], "example.test", mid_validity(), &trust), Err(ChainError::UnknownCa));
    }

    #[test]
    fn self_signed_rsa_anchored_to_itself() {
        // Een zelfondertekende cert die we expliciet vertrouwen valideert.
        let trust = TrustStore::from_ders(&[RSA]);
        let rsa = Certificate::parse(RSA).unwrap();
        let now = rsa.not_before + 100;
        assert!(validate(&[RSA], "rsa.example.test", now, &trust).is_ok());
        assert_eq!(validate(&[RSA], "other.test", now, &trust), Err(ChainError::HostnameMismatch));
    }

    #[test]
    fn empty_chain_rejected() {
        let trust = TrustStore::from_ders(&[EC_ROOT]);
        assert_eq!(validate(&[], "example.test", mid_validity(), &trust), Err(ChainError::EmptyChain));
    }

    #[test]
    fn garbage_cert_in_chain_is_parse_error() {
        let trust = TrustStore::from_ders(&[EC_ROOT]);
        assert!(matches!(
            validate(&[&[0xFF; 40]], "example.test", mid_validity(), &trust),
            Err(ChainError::Parse(_))
        ));
    }
}

#[cfg(test)]
mod realworld_tests {
    use super::*;
    use crate::x509::{Certificate, PubKeyAlg, SigAlg};

    const SSLCOM_ROOT: &[u8] = include_bytes!("../testdata/sslcom_root.der");
    const SSLCOM_TRANSIT: &[u8] = include_bytes!("../testdata/sslcom_transit.der");
    const COMODO_AAA: &[u8] = include_bytes!("../testdata/comodo_aaa.der");

    #[test]
    fn real_roots_parse() {
        let r = Certificate::parse(SSLCOM_ROOT).expect("SSL.com root parse");
        assert_eq!(r.pubkey_alg, PubKeyAlg::EcP384);
        assert!(r.is_ca, "SSL.com root should be CA");
        let c = Certificate::parse(COMODO_AAA).expect("Comodo AAA parse");
        assert!(c.is_ca, "Comodo AAA should be CA");
        let t = Certificate::parse(SSLCOM_TRANSIT).expect("transit parse");
        assert_eq!(t.sig_alg, SigAlg::EcdsaSha384);
    }

    #[test]
    fn transit_verifies_against_sslcom_root() {
        let root = Certificate::parse(SSLCOM_ROOT).unwrap();
        let transit = Certificate::parse(SSLCOM_TRANSIT).unwrap();
        // De namen moeten koppelen.
        assert_eq!(transit.issuer_der, root.subject_der, "issuer != root subject");
        // En de handtekening moet verifiëren.
        assert!(
            crate::sig::verify(transit.sig_alg, root.pubkey_alg, root.pubkey, transit.tbs_der, transit.signature),
            "transit signature did not verify against SSL.com root"
        );
    }
}
