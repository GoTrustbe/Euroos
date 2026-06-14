//! Bundled trusted root CAs for EuroTLS certificate validation (plan A1).
//! EU-first selection + the major international CAs, as `&'static` DER. Auto-
//! generated from the system CA store (openssl x509 -outform DER).
//!
//! NO manual edits — regenerate via the script in the A1 workflow.

/// The trusted root certificates (DER), passed to `Tls13Client::set_trust_anchor`.
pub static ROOTS: &[&[u8]] = &[
    include_bytes!("tls_roots/Buypass_Class_2_Root_CA.der"),
    include_bytes!("tls_roots/Buypass_Class_3_Root_CA.der"),
    include_bytes!("tls_roots/Certigna.der"),
    include_bytes!("tls_roots/Comodo_AAA_Services_root.der"),
    include_bytes!("tls_roots/DigiCert_Global_Root_CA.der"),
    include_bytes!("tls_roots/DigiCert_Global_Root_G2.der"),
    include_bytes!("tls_roots/DigiCert_Global_Root_G3.der"),
    include_bytes!("tls_roots/DigiCert_TLS_ECC_P384_Root_G5.der"),
    include_bytes!("tls_roots/DigiCert_TLS_RSA4096_Root_G5.der"),
    include_bytes!("tls_roots/D-TRUST_BR_Root_CA_1_2020.der"),
    include_bytes!("tls_roots/D-TRUST_EV_Root_CA_1_2020.der"),
    include_bytes!("tls_roots/D-TRUST_Root_Class_3_CA_2_2009.der"),
    include_bytes!("tls_roots/GlobalSign_ECC_Root_CA_-_R5.der"),
    include_bytes!("tls_roots/GlobalSign_Root_CA.der"),
    include_bytes!("tls_roots/GlobalSign_Root_CA_-_R3.der"),
    include_bytes!("tls_roots/GlobalSign_Root_R46.der"),
    include_bytes!("tls_roots/ISRG_Root_X1.der"),
    include_bytes!("tls_roots/ISRG_Root_X2.der"),
    include_bytes!("tls_roots/QuoVadis_Root_CA_2.der"),
    include_bytes!("tls_roots/QuoVadis_Root_CA_2_G3.der"),
    include_bytes!("tls_roots/QuoVadis_Root_CA_3_G3.der"),
    include_bytes!("tls_roots/SSL.com_EV_Root_Certification_Authority_ECC.der"),
    include_bytes!("tls_roots/SSL.com_EV_Root_Certification_Authority_RSA_R2.der"),
    include_bytes!("tls_roots/SSL.com_Root_Certification_Authority_ECC.der"),
    include_bytes!("tls_roots/SSL.com_Root_Certification_Authority_RSA.der"),
    include_bytes!("tls_roots/SSL.com_TLS_ECC_Root_CA_2022.der"),
    include_bytes!("tls_roots/SSL.com_TLS_RSA_Root_CA_2022.der"),
    include_bytes!("tls_roots/SwissSign_Gold_CA_-_G2.der"),
    include_bytes!("tls_roots/USERTrust_ECC_Certification_Authority.der"),
    include_bytes!("tls_roots/USERTrust_RSA_Certification_Authority.der"),
];
