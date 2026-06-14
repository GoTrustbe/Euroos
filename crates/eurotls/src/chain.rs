//! X.509 chain validation (plan A1, phase 3): bind a server-offered
//! certificate chain to a trusted root, with validity and hostname checks.
//! A simplified RFC 5280 §6.1 path validation: name chaining + signature
//! per step + basicConstraints(CA) on intermediate certificates + time window + SAN.
//!
//! No panic on garbage; every deviation yields a `ChainError`.

use alloc::vec::Vec;

use crate::sig;
use crate::x509::{Certificate, PubKeyAlg, X509Error};

/// Reason why a chain could not be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainError {
    /// The chain was empty.
    EmptyChain,
    /// A certificate could not be parsed.
    Parse(X509Error),
    /// A certificate is expired or not yet valid at the reference time.
    Expired,
    /// The hostname matches none of the SubjectAltName dNSNames.
    HostnameMismatch,
    /// issuer (child) ≠ subject (issuer): the chain is not contiguous.
    BrokenChain,
    /// An issuer in the chain is not a CA (basicConstraints CA:TRUE missing).
    IssuerNotCa,
    /// A signature in the chain did not verify.
    BadSignature,
    /// None of the chain anchor points is in the trust store.
    UnknownCa,
}

impl From<X509Error> for ChainError {
    fn from(e: X509Error) -> Self {
        ChainError::Parse(e)
    }
}

/// A collection of trusted root CAs. Borrows the DER bytes (`'a`), so the
/// kernel can offer a `'static` bundled EU/Mozilla store without a copy.
pub struct TrustStore<'a> {
    roots: Vec<Certificate<'a>>,
}

impl<'a> TrustStore<'a> {
    /// Build a trust store from DER-encoded root certificates. Unreadable
    /// roots are skipped (a broken bundle entry does not wreck the store).
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

    /// Find a trusted root whose subject name equals `issuer`.
    fn find_issuer(&self, issuer: &[u8]) -> Option<&Certificate<'a>> {
        self.roots.iter().find(|r| r.subject_der == issuer)
    }
}

/// Validation result: the verified leaf key material, ready to check the
/// server signature in the TLS handshake (CertificateVerify).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub leaf_pubkey_alg: PubKeyAlg,
    pub leaf_pubkey: Vec<u8>,
}

/// Validate a certificate chain (leaf first, then intermediates; the
/// root belongs in the trust store, not in the chain). `now` = epoch seconds.
pub fn validate(chain_der: &[&[u8]], hostname: &str, now: i64, trust: &TrustStore) -> Result<Verified, ChainError> {
    if chain_der.is_empty() {
        return Err(ChainError::EmptyChain);
    }
    // Parse the entire chain.
    let mut certs: Vec<Certificate> = Vec::with_capacity(chain_der.len());
    for der in chain_der {
        certs.push(Certificate::parse(der)?);
    }

    // Leaf: time window + hostname.
    let leaf = &certs[0];
    if !leaf.valid_at(now) {
        return Err(ChainError::Expired);
    }
    if !leaf.matches_hostname(hostname) {
        return Err(ChainError::HostnameMismatch);
    }

    // Loop through the chain and try to anchor at each certificate as soon as its
    // issuer is a trusted root. This way extra cross-sign certificates that
    // the server sends on top of an already trusted intermediate root can be ignored
    // (e.g. ISRG Root X2 cross-signed by X1, while X2 itself is already trusted).
    for i in 0..certs.len() {
        let cert = &certs[i];
        // Anchor: is the issuer of this certificate a trusted root?
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
        // Otherwise: this certificate must be signed by the next one in the chain,
        // and that next one must be a valid CA.
        if i + 1 >= certs.len() {
            return Err(ChainError::UnknownCa); // no anchor found
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

    // A reference time that surely falls within the validity window of the leaf.
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
        // The second SAN matches too.
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
        // The leaf itself as "root" → its subject ≠ the issuer name of the leaf.
        let trust = TrustStore::from_ders(&[EC_LEAF]);
        assert_eq!(validate(&[EC_LEAF], "example.test", mid_validity(), &trust), Err(ChainError::UnknownCa));
    }

    #[test]
    fn self_signed_rsa_anchored_to_itself() {
        // A self-signed cert that we explicitly trust validates.
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
        // The names must chain.
        assert_eq!(transit.issuer_der, root.subject_der, "issuer != root subject");
        // And the signature must verify.
        assert!(
            crate::sig::verify(transit.sig_alg, root.pubkey_alg, root.pubkey, transit.tbs_der, transit.signature),
            "transit signature did not verify against SSL.com root"
        );
    }
}
