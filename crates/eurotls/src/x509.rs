//! Minimal DER/ASN.1 parser + X.509 v3 certificate parsing (RFC 5280) for
//! EuroTLS certificate validation (plan A1, phase 1). Pure `no_std` + `alloc`, no
//! external dependencies. **Security requirement: never panic on untrusted
//! bytes** — every access is bounds-checked and every error is returned cleanly.
//!
//! This module only *parses* (structure + fields). Signature verification,
//! chain building and the trust store follow in later phases and build on this.

use alloc::vec::Vec;

/// Parse error. No variant may lead to a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X509Error {
    /// Unexpected end of input.
    Truncated,
    /// Wrong ASN.1 tag where a specific one was expected.
    UnexpectedTag,
    /// Invalid length encoding (indefinite form, non-minimal, too large).
    BadLength,
    /// Structure does not match the X.509 schema.
    Malformed,
    /// A date/time could not be parsed.
    BadTime,
}

type R<T> = Result<T, X509Error>;

// ── ASN.1 tags ─────────────────────────────────────────────────────────────
pub const T_INTEGER: u8 = 0x02;
pub const T_BIT_STRING: u8 = 0x03;
pub const T_OCTET_STRING: u8 = 0x04;
pub const T_NULL: u8 = 0x05;
pub const T_OID: u8 = 0x06;
pub const T_UTF8STRING: u8 = 0x0C;
pub const T_PRINTABLESTRING: u8 = 0x13;
pub const T_IA5STRING: u8 = 0x16;
pub const T_UTCTIME: u8 = 0x17;
pub const T_GENERALIZEDTIME: u8 = 0x18;
pub const T_SEQUENCE: u8 = 0x30;
pub const T_SET: u8 = 0x31;
const T_BOOLEAN: u8 = 0x01;
// Context-specific, constructed tags in TBSCertificate.
const T_CTX0: u8 = 0xA0; // [0] version
const T_CTX3: u8 = 0xA3; // [3] extensions
// dNSName within SubjectAltName = [2] IA5String (primitive).
const T_SAN_DNS: u8 = 0x82;

// ── Known OIDs (DER-encoded value bytes, without tag/length) ──────────
pub const OID_EC_PUBLIC_KEY: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01]; // 1.2.840.10045.2.1
pub const OID_P256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07]; // 1.2.840.10045.3.1.7
pub const OID_P384: &[u8] = &[0x2B, 0x81, 0x04, 0x00, 0x22]; // 1.3.132.0.34 (secp384r1)
pub const OID_ECDSA_SHA256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02]; // 1.2.840.10045.4.3.2
pub const OID_ECDSA_SHA384: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x03]; // 1.2.840.10045.4.3.3
pub const OID_RSA_ENCRYPTION: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01]; // 1.2.840.113549.1.1.1
pub const OID_SHA256_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B]; // …1.1.11
pub const OID_SHA384_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0C]; // …1.1.12
pub const OID_ED25519: &[u8] = &[0x2B, 0x65, 0x70]; // 1.3.101.112
const OID_SAN: &[u8] = &[0x55, 0x1D, 0x11]; // 2.5.29.17
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1D, 0x13]; // 2.5.29.19
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1D, 0x0F]; // 2.5.29.15

// ── DER cursor ───────────────────────────────────────────────────────────
/// One parsed TLV element: tag, the full element bytes (incl. header, for
/// hashing) and the content (the value only).
struct Tlv<'a> {
    tag: u8,
    full: &'a [u8],
    content: &'a [u8],
}

/// Read one TLV from the front of `input`; return the element + the rest.
fn take<'a>(input: &'a [u8]) -> R<(Tlv<'a>, &'a [u8])> {
    if input.len() < 2 {
        return Err(X509Error::Truncated);
    }
    let tag = input[0];
    let len_byte = input[1];
    let (len, header) = if len_byte < 0x80 {
        (len_byte as usize, 2usize)
    } else if len_byte == 0x80 {
        return Err(X509Error::BadLength); // indefinite form is forbidden in DER
    } else {
        let n = (len_byte & 0x7F) as usize;
        if n == 0 || n > 4 {
            return Err(X509Error::BadLength);
        }
        if input.len() < 2 + n {
            return Err(X509Error::Truncated);
        }
        let mut l = 0usize;
        for &b in &input[2..2 + n] {
            l = (l << 8) | b as usize;
        }
        // Reject non-minimal length encoding (DER requirement).
        if l < 0x80 {
            return Err(X509Error::BadLength);
        }
        (l, 2 + n)
    };
    let end = header.checked_add(len).ok_or(X509Error::BadLength)?;
    if input.len() < end {
        return Err(X509Error::Truncated);
    }
    let tlv = Tlv { tag, full: &input[..end], content: &input[header..end] };
    Ok((tlv, &input[end..]))
}

/// Read one TLV and require a specific tag.
fn expect<'a>(input: &'a [u8], tag: u8) -> R<(Tlv<'a>, &'a [u8])> {
    let (tlv, rest) = take(input)?;
    if tlv.tag != tag {
        return Err(X509Error::UnexpectedTag);
    }
    Ok((tlv, rest))
}

/// The BIT STRING content without the leading "unused bits" byte.
fn bit_string_bytes(content: &[u8]) -> R<&[u8]> {
    // First byte = number of unused bits (in certs always 0 for keys/sigs).
    if content.is_empty() {
        return Err(X509Error::Malformed);
    }
    Ok(&content[1..])
}

// ── Time ────────────────────────────────────────────────────────────────
/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - (m <= 2) as i64;
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse UTCTime (YYMMDDHHMMSSZ) or GeneralizedTime (YYYYMMDDHHMMSSZ) into
/// epoch seconds (UTC). Only the Zulu form is accepted (as RFC 5280
/// requires for certificates).
pub fn parse_asn1_time(tag: u8, content: &[u8]) -> R<i64> {
    fn num(b: &[u8]) -> R<i64> {
        let mut v = 0i64;
        for &c in b {
            if !c.is_ascii_digit() {
                return Err(X509Error::BadTime);
            }
            v = v * 10 + (c - b'0') as i64;
        }
        Ok(v)
    }
    let (year, rest) = match tag {
        T_UTCTIME => {
            // YYMMDDHHMMSSZ — exactly 13 bytes.
            if content.len() != 13 || content[12] != b'Z' {
                return Err(X509Error::BadTime);
            }
            let yy = num(&content[0..2])?;
            // RFC 5280: YY < 50 → 20YY, otherwise 19YY.
            let year = if yy < 50 { 2000 + yy } else { 1900 + yy };
            (year, &content[2..12])
        }
        T_GENERALIZEDTIME => {
            // YYYYMMDDHHMMSSZ — exactly 15 bytes.
            if content.len() != 15 || content[14] != b'Z' {
                return Err(X509Error::BadTime);
            }
            let year = num(&content[0..4])?;
            (year, &content[4..14])
        }
        _ => return Err(X509Error::UnexpectedTag),
    };
    let mo = num(&rest[0..2])?;
    let da = num(&rest[2..4])?;
    let hh = num(&rest[4..6])?;
    let mi = num(&rest[6..8])?;
    let ss = num(&rest[8..10])?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || hh > 23 || mi > 59 || ss > 60 {
        return Err(X509Error::BadTime);
    }
    let days = days_from_civil(year, mo, da);
    Ok(days * 86400 + hh * 3600 + mi * 60 + ss)
}

// ── Public-key types ────────────────────────────────────────────────
/// The public-key algorithm from the SubjectPublicKeyInfo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PubKeyAlg {
    /// EC P-256 — `key` = uncompressed point (0x04 ‖ X ‖ Y, 65 bytes).
    EcP256,
    /// EC P-384 — `key` = uncompressed point (0x04 ‖ X ‖ Y, 97 bytes).
    EcP384,
    /// RSA — `key` = DER SEQUENCE { modulus INTEGER, exponent INTEGER }.
    Rsa,
    /// Ed25519 — `key` = 32-byte public key.
    Ed25519,
    /// A type we do not (yet) support.
    Unsupported,
}

/// The signature algorithm with which this certificate was signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigAlg {
    EcdsaSha256,
    EcdsaSha384,
    RsaSha256,
    RsaSha384,
    Ed25519,
    Unsupported,
}

fn sig_alg_from_oid(oid: &[u8]) -> SigAlg {
    if oid == OID_ECDSA_SHA256 {
        SigAlg::EcdsaSha256
    } else if oid == OID_ECDSA_SHA384 {
        SigAlg::EcdsaSha384
    } else if oid == OID_SHA256_RSA {
        SigAlg::RsaSha256
    } else if oid == OID_SHA384_RSA {
        SigAlg::RsaSha384
    } else if oid == OID_ED25519 {
        SigAlg::Ed25519
    } else {
        SigAlg::Unsupported
    }
}

// ── The parsed certificate ────────────────────────────────────────────────
/// A parsed X.509 v3 certificate. All fields borrow from the original
/// DER buffer (`'a`), so nothing is copied except the SAN list.
#[derive(Debug, Clone)]
pub struct Certificate<'a> {
    /// The raw `tbsCertificate` (incl. SEQUENCE header) — *this* is what is signed.
    pub tbs_der: &'a [u8],
    /// serialNumber bytes (INTEGER content).
    pub serial: &'a [u8],
    /// Raw issuer `Name` (incl. header) — for issuer/subject linking.
    pub issuer_der: &'a [u8],
    /// Raw subject `Name` (incl. header).
    pub subject_der: &'a [u8],
    /// Validity window in epoch seconds.
    pub not_before: i64,
    pub not_after: i64,
    /// Public-key algorithm + raw key bytes.
    pub pubkey_alg: PubKeyAlg,
    pub pubkey: &'a [u8],
    /// Signature algorithm (outer `signatureAlgorithm`).
    pub sig_alg: SigAlg,
    /// Signature value (BIT STRING content, without unused-bits byte).
    pub signature: &'a [u8],
    /// SubjectAltName dNSName entries (UTF-8/ASCII).
    pub san_dns: Vec<&'a str>,
    /// basicConstraints: is this a CA certificate?
    pub is_ca: bool,
    /// basicConstraints present?
    pub basic_constraints_present: bool,
}

impl<'a> Certificate<'a> {
    /// Parse an X.509 certificate from DER. Never panics on garbage.
    pub fn parse(der: &'a [u8]) -> R<Certificate<'a>> {
        // Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signatureValue }
        let (cert_seq, trailing) = expect(der, T_SEQUENCE)?;
        if !trailing.is_empty() {
            return Err(X509Error::Malformed); // bytes after the certificate
        }
        let body = cert_seq.content;

        // tbsCertificate (keep the full bytes for signature verification).
        let (tbs, rest) = expect(body, T_SEQUENCE)?;
        let tbs_der = tbs.full;

        // signatureAlgorithm.
        let (sig_alg_seq, rest) = expect(rest, T_SEQUENCE)?;
        let (sig_oid, _) = expect(sig_alg_seq.content, T_OID)?;
        let sig_alg = sig_alg_from_oid(sig_oid.content);

        // signatureValue.
        let (sig_bits, rest) = expect(rest, T_BIT_STRING)?;
        if !rest.is_empty() {
            return Err(X509Error::Malformed);
        }
        let signature = bit_string_bytes(sig_bits.content)?;

        // ── TBSCertificate fields ──
        let t = tbs.content;
        // [0] version (optional) — skip.
        let (_, t) = match take(t)? {
            (tlv, rest) if tlv.tag == T_CTX0 => ((), rest),
            _ => ((), t), // no version field → v1; leave t unchanged
        };
        // serialNumber INTEGER.
        let (serial_tlv, t) = expect(t, T_INTEGER)?;
        let serial = serial_tlv.content;
        // signature AlgorithmIdentifier — skip (we use the outer one).
        let (_, t) = expect(t, T_SEQUENCE)?;
        // issuer Name.
        let (issuer_tlv, t) = expect(t, T_SEQUENCE)?;
        let issuer_der = issuer_tlv.full;
        // validity SEQUENCE { notBefore, notAfter }.
        let (validity_tlv, t) = expect(t, T_SEQUENCE)?;
        let (nb_tlv, vrest) = take(validity_tlv.content)?;
        let (na_tlv, _) = take(vrest)?;
        let not_before = parse_asn1_time(nb_tlv.tag, nb_tlv.content)?;
        let not_after = parse_asn1_time(na_tlv.tag, na_tlv.content)?;
        // subject Name.
        let (subject_tlv, t) = expect(t, T_SEQUENCE)?;
        let subject_der = subject_tlv.full;
        // subjectPublicKeyInfo SEQUENCE { algorithm, subjectPublicKey }.
        let (spki_tlv, t) = expect(t, T_SEQUENCE)?;
        let (pubkey_alg, pubkey) = parse_spki(spki_tlv.content)?;

        // Skip optional [1]/[2] uniqueIDs until we find [3] extensions.
        let mut san_dns = Vec::new();
        let mut is_ca = false;
        let mut basic_constraints_present = false;
        let mut cursor = t;
        while !cursor.is_empty() {
            let (tlv, rest) = take(cursor)?;
            if tlv.tag == T_CTX3 {
                parse_extensions(tlv.content, &mut san_dns, &mut is_ca, &mut basic_constraints_present)?;
                break;
            }
            cursor = rest;
        }

        Ok(Certificate {
            tbs_der,
            serial,
            issuer_der,
            subject_der,
            not_before,
            not_after,
            pubkey_alg,
            pubkey,
            sig_alg,
            signature,
            san_dns,
            is_ca,
            basic_constraints_present,
        })
    }

    /// True if this certificate is valid at time `now` (epoch seconds).
    pub fn valid_at(&self, now: i64) -> bool {
        self.not_before <= now && now <= self.not_after
    }

    /// True if `host` matches one of the dNSName entries (exact match or
    /// a wildcard `*.example.com` that replaces only the leftmost label).
    pub fn matches_hostname(&self, host: &str) -> bool {
        self.san_dns.iter().any(|pat| dns_name_matches(pat, host))
    }
}

/// Split an RSA public key (`RSAPublicKey ::= SEQUENCE { modulus INTEGER,
/// publicExponent INTEGER }`, the SPKI BIT STRING content) into (modulus, exponent).
/// The INTEGER content may contain a leading 0x00 (sign byte) — we leave it
/// in place; the caller strips leading zeros where needed.
pub fn parse_rsa_public_key(spki_key: &[u8]) -> R<(&[u8], &[u8])> {
    let (seq, _) = expect(spki_key, T_SEQUENCE)?;
    let (modulus, rest) = expect(seq.content, T_INTEGER)?;
    let (exponent, _) = expect(rest, T_INTEGER)?;
    Ok((modulus.content, exponent.content))
}

/// Parse SubjectPublicKeyInfo → (algorithm, raw key bytes).
fn parse_spki(spki: &[u8]) -> R<(PubKeyAlg, &[u8])> {
    let (alg_seq, rest) = expect(spki, T_SEQUENCE)?;
    let (alg_oid, alg_rest) = expect(alg_seq.content, T_OID)?;
    let (key_bits, _) = expect(rest, T_BIT_STRING)?;
    let key = bit_string_bytes(key_bits.content)?;

    if alg_oid.content == OID_EC_PUBLIC_KEY {
        // parameters = named curve OID. P-256 and P-384 are supported.
        let (curve, _) = expect(alg_rest, T_OID)?;
        if curve.content == OID_P256 {
            return Ok((PubKeyAlg::EcP256, key));
        }
        if curve.content == OID_P384 {
            return Ok((PubKeyAlg::EcP384, key));
        }
        return Ok((PubKeyAlg::Unsupported, key));
    }
    if alg_oid.content == OID_RSA_ENCRYPTION {
        return Ok((PubKeyAlg::Rsa, key));
    }
    if alg_oid.content == OID_ED25519 {
        return Ok((PubKeyAlg::Ed25519, key));
    }
    Ok((PubKeyAlg::Unsupported, key))
}

/// Walk through the extensions; fill in SAN dNSNames + basicConstraints(CA).
fn parse_extensions<'a>(
    ctx3: &'a [u8],
    san_dns: &mut Vec<&'a str>,
    is_ca: &mut bool,
    bc_present: &mut bool,
) -> R<()> {
    // [3] EXPLICIT → one SEQUENCE OF Extension.
    let (ext_seq, _) = expect(ctx3, T_SEQUENCE)?;
    let mut cursor = ext_seq.content;
    while !cursor.is_empty() {
        let (ext_tlv, rest) = expect(cursor, T_SEQUENCE)?;
        cursor = rest;
        // Extension ::= SEQUENCE { extnID OID, critical BOOLEAN DEFAULT FALSE, extnValue OCTET STRING }
        let (oid, mut erest) = expect(ext_tlv.content, T_OID)?;
        // Skip the optional critical BOOLEAN.
        let (peek, after_peek) = take(erest)?;
        if peek.tag == T_BOOLEAN {
            erest = after_peek;
        }
        let (val, _) = expect(erest, T_OCTET_STRING)?;
        if oid.content == OID_SAN {
            parse_san(val.content, san_dns)?;
        } else if oid.content == OID_BASIC_CONSTRAINTS {
            *bc_present = true;
            *is_ca = parse_basic_constraints_ca(val.content)?;
        } else if oid.content == OID_KEY_USAGE {
            // (Parsed but not yet enforced — comes in the chain phase.)
        }
    }
    Ok(())
}

/// SubjectAltName ::= GeneralNames ::= SEQUENCE OF GeneralName. We only take
/// dNSName ([2] IA5String).
fn parse_san<'a>(val: &'a [u8], out: &mut Vec<&'a str>) -> R<()> {
    let (seq, _) = expect(val, T_SEQUENCE)?;
    let mut cursor = seq.content;
    while !cursor.is_empty() {
        let (tlv, rest) = take(cursor)?;
        cursor = rest;
        if tlv.tag == T_SAN_DNS {
            if let Ok(s) = core::str::from_utf8(tlv.content) {
                out.push(s);
            }
        }
    }
    Ok(())
}

/// basicConstraints ::= SEQUENCE { cA BOOLEAN DEFAULT FALSE, pathLen INTEGER OPTIONAL }
fn parse_basic_constraints_ca(val: &[u8]) -> R<bool> {
    let (seq, _) = expect(val, T_SEQUENCE)?;
    if seq.content.is_empty() {
        return Ok(false); // cA defaults to FALSE
    }
    let (first, _) = take(seq.content)?;
    if first.tag == T_BOOLEAN {
        return Ok(first.content.first().is_some_and(|&b| b != 0));
    }
    Ok(false)
}

/// Hostname matching with wildcard support on the leftmost label.
fn dns_name_matches(pattern: &str, host: &str) -> bool {
    // Case-insensitive; no trailing-dot mismatch.
    let p = pattern.trim_end_matches('.');
    let h = host.trim_end_matches('.');
    if let Some(suffix) = p.strip_prefix("*.") {
        // Wildcard matches exactly one label on the left; the suffix must match
        // exactly and the host must have at least one label before the suffix.
        if let Some(host_rest) = h.split_once('.').map(|(_, rest)| rest) {
            return !host_rest.is_empty() && host_rest.eq_ignore_ascii_case(suffix);
        }
        return false;
    }
    p.eq_ignore_ascii_case(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EC_ROOT: &[u8] = include_bytes!("../testdata/ec_root.der");
    const EC_LEAF: &[u8] = include_bytes!("../testdata/ec_leaf.der");
    const RSA: &[u8] = include_bytes!("../testdata/rsa.der");

    #[test]
    fn parse_ec_leaf_fields() {
        let c = Certificate::parse(EC_LEAF).unwrap();
        assert_eq!(c.pubkey_alg, PubKeyAlg::EcP256);
        assert_eq!(c.sig_alg, SigAlg::EcdsaSha256);
        // Uncompressed P-256 point: 0x04 ‖ 32 ‖ 32 = 65 bytes.
        assert_eq!(c.pubkey.len(), 65);
        assert_eq!(c.pubkey[0], 0x04);
        // SAN dNSNames from the fixture.
        assert!(c.san_dns.contains(&"example.test"));
        assert!(c.san_dns.contains(&"www.example.test"));
        // Validity window is consistent.
        assert!(c.not_before < c.not_after);
        // tbs_der starts with a SEQUENCE tag and is shorter than the whole cert.
        assert_eq!(c.tbs_der[0], T_SEQUENCE);
        assert!(c.tbs_der.len() < EC_LEAF.len());
        // A leaf is not a CA.
        assert!(!c.is_ca);
    }

    #[test]
    fn parse_ec_root_is_ca() {
        let c = Certificate::parse(EC_ROOT).unwrap();
        assert_eq!(c.pubkey_alg, PubKeyAlg::EcP256);
        // A self-signed root: issuer == subject.
        assert_eq!(c.issuer_der, c.subject_der);
        // X.509 v3 root CAs carry basicConstraints CA:TRUE.
        assert!(c.is_ca);
    }

    #[test]
    fn parse_rsa_cert_fields() {
        let c = Certificate::parse(RSA).unwrap();
        assert_eq!(c.pubkey_alg, PubKeyAlg::Rsa);
        assert_eq!(c.sig_alg, SigAlg::RsaSha256);
        // RSA-2048 SPKI key = SEQUENCE { modulus(~257), exponent } → ~270 bytes.
        assert!(c.pubkey.len() > 256 && c.pubkey.len() < 300);
        assert_eq!(c.pubkey[0], T_SEQUENCE);
        assert!(c.san_dns.contains(&"rsa.example.test"));
    }

    #[test]
    fn hostname_wildcard_matching() {
        // Exact match.
        assert!(dns_name_matches("example.test", "example.test"));
        assert!(dns_name_matches("Example.Test", "example.test")); // case-insensitive
        assert!(!dns_name_matches("example.test", "evil.test"));
        // Wildcard matches exactly one label on the left.
        assert!(dns_name_matches("*.example.com", "www.example.com"));
        assert!(!dns_name_matches("*.example.com", "example.com")); // no label
        assert!(!dns_name_matches("*.example.com", "a.b.example.com")); // two labels
        assert!(!dns_name_matches("*.example.com", "www.evil.com"));
    }

    #[test]
    fn matches_hostname_uses_san() {
        let c = Certificate::parse(EC_LEAF).unwrap();
        assert!(c.matches_hostname("example.test"));
        assert!(c.matches_hostname("www.example.test"));
        assert!(!c.matches_hostname("evil.test"));
    }

    #[test]
    fn time_parsing_is_exact() {
        // UTCTime 2025-01-02 03:04:05Z. Known epoch: 1735787045.
        let utc = b"250102030405Z";
        assert_eq!(parse_asn1_time(T_UTCTIME, utc).unwrap(), 1_735_787_045);
        // GeneralizedTime, same instant.
        let gen = b"20250102030405Z";
        assert_eq!(parse_asn1_time(T_GENERALIZEDTIME, gen).unwrap(), 1_735_787_045);
        // YY < 50 → 20YY; YY >= 50 → 19YY.
        assert_eq!(
            parse_asn1_time(T_UTCTIME, b"700101000000Z").unwrap(),
            parse_asn1_time(T_GENERALIZEDTIME, b"19700101000000Z").unwrap()
        );
        // Unix epoch itself.
        assert_eq!(parse_asn1_time(T_GENERALIZEDTIME, b"19700101000000Z").unwrap(), 0);
    }

    #[test]
    fn time_parsing_rejects_garbage() {
        assert_eq!(parse_asn1_time(T_UTCTIME, b"25010203040"), Err(X509Error::BadTime)); // too short
        assert_eq!(parse_asn1_time(T_UTCTIME, b"2501020304XZ"), Err(X509Error::BadTime)); // too short/non-digit
        assert_eq!(parse_asn1_time(T_UTCTIME, b"251302030405Z"), Err(X509Error::BadTime)); // month 13
        assert_eq!(parse_asn1_time(T_GENERALIZEDTIME, b"20250102030405X"), Err(X509Error::BadTime)); // no Z
    }

    #[test]
    fn never_panics_on_truncated_input() {
        // Every prefix of a real certificate must return an Err, never a panic.
        for n in 0..EC_LEAF.len() {
            let _ = Certificate::parse(&EC_LEAF[..n]);
        }
        // Random garbage.
        assert!(Certificate::parse(&[]).is_err());
        assert!(Certificate::parse(&[0x30]).is_err());
        assert!(Certificate::parse(&[0x30, 0x80]).is_err()); // indefinite length
        assert!(Certificate::parse(&[0xFF; 64]).is_err());
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut buf = EC_LEAF.to_vec();
        buf.push(0x00); // extra byte after the certificate
        assert!(matches!(Certificate::parse(&buf), Err(X509Error::Malformed)));
    }
}
