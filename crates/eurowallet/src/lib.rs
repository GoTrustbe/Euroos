//! EuroWallet — **eIDAS 2.0 / EUDI-wallet credentials** for EuroOS.
//!
//! Implements **SD-JWT VC** (IETF Selective Disclosure JWT for Verifiable
//! Credentials), the format the European Digital Identity Wallet uses. An
//! issuer (e.g. a member-state PID provider, or EuroID acting as one) signs a
//! credential in which each attribute is **selectively disclosable**: the holder
//! later reveals only the claims a relying party needs (say "over 18" or
//! "nationality") while the signature over the whole credential still verifies,
//! and the undisclosed attributes stay hidden as salted digests.
//!
//! Signatures are **EdDSA (Ed25519)** — sovereign, already in the EuroOS crypto
//! stack. The disclosure/digest encoding is cross-checked against the IETF
//! SD-JWT reference values (`tests`), so credentials interoperate.
//!
//! Scope: the software half of sprint 3D-10 (issue / present / verify + holder
//! key binding). The Belgian eID **card-reader** half is hardware-gated.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod b64;
pub mod json;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use json::Json;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletError {
    Malformed,
    BadSignature,
    /// A presented disclosure whose digest is not committed in the credential
    /// (a forged/injected claim) — the whole presentation is rejected.
    ForgedDisclosure,
    UnsupportedAlg,
    /// Holder key binding present but invalid (wrong holder, audience or nonce).
    BadKeyBinding,
}

/// A single selective-disclosure item: `base64url(["salt", "name", "value"])`.
pub struct Disclosure {
    pub name: String,
    pub encoded: String,
}

/// Build a disclosure exactly as the IETF SD-JWT spec serializes it
/// (`["salt", "name", "value"]` with `, ` separators, base64url of the UTF-8).
pub fn make_disclosure(salt: &str, name: &str, value: &str) -> Disclosure {
    let arr = Json::Arr(alloc::vec![
        Json::Str(salt.to_string()),
        Json::Str(name.to_string()),
        Json::Str(value.to_string()),
    ]);
    let encoded = b64::encode(json::serialize(&arr).as_bytes());
    Disclosure { name: name.to_string(), encoded }
}

/// The `_sd` digest of a disclosure: `base64url(SHA-256(ascii(disclosure)))`.
pub fn sd_digest(encoded_disclosure: &str) -> String {
    let mut h = Sha256::new();
    h.update(encoded_disclosure.as_bytes());
    b64::encode(&h.finalize())
}

fn b64_json(v: &Json) -> String {
    b64::encode(json::serialize(v).as_bytes())
}

/// Issue an SD-JWT VC. `plain` claims are always visible (e.g. `iss`, `vct`,
/// `iat`, `cnf`); `sd` claims (each a `(salt, name, value)`) are selectively
/// disclosable. Returns the compact SD-JWT: `<jwt>~<disc>~...~`.
pub fn issue(
    signing_key: &SigningKey,
    plain: &[(&str, Json)],
    sd: &[(&str, &str, &str)],
) -> String {
    let mut disclosures = Vec::new();
    let mut digests = Vec::new();
    for (salt, name, value) in sd {
        let d = make_disclosure(salt, name, value);
        digests.push(Json::Str(sd_digest(&d.encoded)));
        disclosures.push(d.encoded);
    }

    let mut payload: Vec<(String, Json)> = plain.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    payload.push(("_sd".to_string(), Json::Arr(digests)));
    payload.push(("_sd_alg".to_string(), Json::Str("sha-256".to_string())));
    let payload = Json::Obj(payload);

    let header = Json::Obj(alloc::vec![
        ("alg".to_string(), Json::Str("EdDSA".to_string())),
        ("typ".to_string(), Json::Str("dc+sd-jwt".to_string())),
    ]);

    let signing_input = alloc::format!("{}.{}", b64_json(&header), b64_json(&payload));
    let sig = signing_key.sign(signing_input.as_bytes()).to_bytes();
    let jwt = alloc::format!("{}.{}", signing_input, b64::encode(&sig));

    let mut out = jwt;
    for d in &disclosures {
        out.push('~');
        out.push_str(d);
    }
    out.push('~');
    out
}

/// Produce a **presentation** that reveals only the named claims: drop every
/// disclosure whose claim name is not in `reveal`. The issuer signature is
/// untouched, so it still verifies over the (unchanged) credential body.
pub fn present(sd_jwt: &str, reveal: &[&str]) -> Option<String> {
    let mut parts = sd_jwt.split('~');
    let jwt = parts.next()?;
    let mut out = String::from(jwt);
    for p in parts {
        if p.is_empty() {
            continue;
        }
        let raw = b64::decode(p)?;
        let s = core::str::from_utf8(&raw).ok()?;
        let arr = json::parse(s)?;
        let name = arr.as_array()?.get(1)?.as_str()?;
        if reveal.contains(&name) {
            out.push('~');
            out.push_str(p);
        }
    }
    out.push('~');
    Some(out)
}

/// A verified credential: the issuer-attested claims that were actually
/// disclosed (undisclosed attributes never appear here).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VerifiedCredential {
    pub claims: Vec<(String, String)>,
}
impl VerifiedCredential {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.claims.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

fn split_presentation(pres: &str) -> Option<(&str, Vec<&str>, Option<&str>)> {
    // <jwt>~<d1>~..~<dn>~[<kb-jwt>]. A trailing '~' means no key binding.
    let mut segs: Vec<&str> = pres.split('~').collect();
    let jwt = segs.first().copied()?;
    let kb = if let Some(last) = segs.last() {
        if last.is_empty() {
            segs.pop();
            None
        } else {
            segs.pop() // the KB-JWT
        }
    } else {
        None
    };
    // `segs.get(1..)` (not `segs[1..]`): after popping the KB-JWT candidate the
    // vec can be empty, and a bare `[1..]` slice would panic on empty input
    // (found by the eurofuzz harness).
    let disclosures: Vec<&str> = segs.get(1..).unwrap_or(&[]).iter().copied().filter(|s| !s.is_empty()).collect();
    Some((jwt, disclosures, kb))
}

/// Verify the issuer signature and reconstruct the disclosed claims. Rejects a
/// presentation that injects a disclosure not committed by the issuer.
pub fn verify(presentation: &str, issuer_pub: &VerifyingKey) -> Result<VerifiedCredential, WalletError> {
    let (jwt, disclosures, _kb) = split_presentation(presentation).ok_or(WalletError::Malformed)?;
    verify_issuer(jwt, &disclosures, issuer_pub)
}

fn verify_issuer(jwt: &str, disclosures: &[&str], issuer_pub: &VerifyingKey) -> Result<VerifiedCredential, WalletError> {
    let mut jp = jwt.split('.');
    let h = jp.next().ok_or(WalletError::Malformed)?;
    let p = jp.next().ok_or(WalletError::Malformed)?;
    let s = jp.next().ok_or(WalletError::Malformed)?;
    if jp.next().is_some() {
        return Err(WalletError::Malformed);
    }
    // (1) issuer signature over "header.payload".
    let sig_bytes = b64::decode(s).ok_or(WalletError::Malformed)?;
    let sig_arr: [u8; 64] = sig_bytes.as_slice().try_into().map_err(|_| WalletError::Malformed)?;
    let signing_input = alloc::format!("{h}.{p}");
    issuer_pub
        .verify(signing_input.as_bytes(), &Signature::from_bytes(&sig_arr))
        .map_err(|_| WalletError::BadSignature)?;

    // (2) parse payload, collect the committed _sd digest set + alg.
    let payload_raw = b64::decode(p).ok_or(WalletError::Malformed)?;
    let payload = json::parse(core::str::from_utf8(&payload_raw).map_err(|_| WalletError::Malformed)?)
        .ok_or(WalletError::Malformed)?;
    if payload.get("_sd_alg").and_then(|v| v.as_str()) != Some("sha-256") {
        return Err(WalletError::UnsupportedAlg);
    }
    let committed: Vec<&str> = payload
        .get("_sd")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|d| d.as_str()).collect())
        .unwrap_or_default();

    // (3) every presented disclosure must be committed; then it is revealed.
    let mut claims = Vec::new();
    for enc in disclosures {
        let digest = sd_digest(enc);
        if !committed.contains(&digest.as_str()) {
            return Err(WalletError::ForgedDisclosure);
        }
        let raw = b64::decode(enc).ok_or(WalletError::Malformed)?;
        let arr = json::parse(core::str::from_utf8(&raw).map_err(|_| WalletError::Malformed)?)
            .ok_or(WalletError::Malformed)?;
        let a = arr.as_array().ok_or(WalletError::Malformed)?;
        let name = a.get(1).and_then(|v| v.as_str()).ok_or(WalletError::Malformed)?;
        let value = a.get(2).and_then(|v| v.as_str()).ok_or(WalletError::Malformed)?;
        claims.push((name.to_string(), value.to_string()));
    }

    // (4) expose the always-visible string claims (iss / vct / etc.).
    if let Json::Obj(o) = &payload {
        for (k, v) in o {
            if k == "_sd" || k == "_sd_alg" || k == "cnf" {
                continue;
            }
            if let Json::Str(sv) = v {
                claims.push((k.clone(), sv.clone()));
            }
        }
    }
    Ok(VerifiedCredential { claims })
}

/// The `sd_hash` a holder signs for key binding: `base64url(SHA-256(<everything
/// up to and including the last disclosure '~'>))`.
pub fn sd_hash(jwt_and_disclosures: &str) -> String {
    let mut h = Sha256::new();
    h.update(jwt_and_disclosures.as_bytes());
    b64::encode(&h.finalize())
}

/// Holder key binding: sign `(aud, nonce, sd_hash)` with the holder key, append
/// the KB-JWT to a presentation. Proves the presenter controls the credential's
/// bound key (anti-replay), which a real relying party requires.
pub fn add_key_binding(presentation_no_kb: &str, holder: &SigningKey, aud: &str, nonce: &str) -> String {
    let sdh = sd_hash(presentation_no_kb);
    let header = Json::Obj(alloc::vec![
        ("alg".to_string(), Json::Str("EdDSA".to_string())),
        ("typ".to_string(), Json::Str("kb+jwt".to_string())),
    ]);
    let payload = Json::Obj(alloc::vec![
        ("aud".to_string(), Json::Str(aud.to_string())),
        ("nonce".to_string(), Json::Str(nonce.to_string())),
        ("sd_hash".to_string(), Json::Str(sdh)),
    ]);
    let signing_input = alloc::format!("{}.{}", b64_json(&header), b64_json(&payload));
    let sig = holder.sign(signing_input.as_bytes()).to_bytes();
    alloc::format!("{}{}.{}", presentation_no_kb, signing_input, b64::encode(&sig))
}

/// Verify issuer signature, disclosures, **and** holder key binding against the
/// holder public key + expected audience/nonce. Full relying-party check.
pub fn verify_with_key_binding(
    presentation: &str,
    issuer_pub: &VerifyingKey,
    holder_pub: &VerifyingKey,
    expect_aud: &str,
    expect_nonce: &str,
) -> Result<VerifiedCredential, WalletError> {
    let (jwt, disclosures, kb) = split_presentation(presentation).ok_or(WalletError::Malformed)?;
    let cred = verify_issuer(jwt, &disclosures, issuer_pub)?;
    let kb = kb.ok_or(WalletError::BadKeyBinding)?;

    // Recompute the presentation-without-KB (jwt + disclosures + trailing '~').
    let mut no_kb = String::from(jwt);
    for d in &disclosures {
        no_kb.push('~');
        no_kb.push_str(d);
    }
    no_kb.push('~');

    let mut kp = kb.split('.');
    let h = kp.next().ok_or(WalletError::BadKeyBinding)?;
    let p = kp.next().ok_or(WalletError::BadKeyBinding)?;
    let s = kp.next().ok_or(WalletError::BadKeyBinding)?;
    let sig_bytes = b64::decode(s).ok_or(WalletError::BadKeyBinding)?;
    let sig_arr: [u8; 64] = sig_bytes.as_slice().try_into().map_err(|_| WalletError::BadKeyBinding)?;
    holder_pub
        .verify(alloc::format!("{h}.{p}").as_bytes(), &Signature::from_bytes(&sig_arr))
        .map_err(|_| WalletError::BadKeyBinding)?;

    let payload = json::parse(
        core::str::from_utf8(&b64::decode(p).ok_or(WalletError::BadKeyBinding)?).map_err(|_| WalletError::BadKeyBinding)?,
    )
    .ok_or(WalletError::BadKeyBinding)?;
    let ok = payload.get("aud").and_then(|v| v.as_str()) == Some(expect_aud)
        && payload.get("nonce").and_then(|v| v.as_str()) == Some(expect_nonce)
        && payload.get("sd_hash").and_then(|v| v.as_str()) == Some(sd_hash(&no_kb).as_str());
    if !ok {
        return Err(WalletError::BadKeyBinding);
    }
    Ok(cred)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ietf_reference_disclosure_and_digest() {
        // Cross-check against the IETF SD-JWT specification's own worked example.
        let d = make_disclosure("2GLC42sKQveCfGfryNRN9w", "given_name", "John");
        assert_eq!(d.encoded, "WyIyR0xDNDJzS1F2ZUNmR2ZyeU5STjl3IiwgImdpdmVuX25hbWUiLCAiSm9obiJd");
        assert_eq!(sd_digest(&d.encoded), "jsu9yVulwQQlhFlM_3JlzMaSFzglhQG0DpfayQwLUK4");
    }

    fn issuer() -> SigningKey {
        SigningKey::from_bytes(&[0x11u8; 32])
    }

    fn sample() -> String {
        issue(
            &issuer(),
            &[("iss", Json::Str("https://euro-id.eu".into())), ("vct", Json::Str("eu.europa.ec.eudi.pid.1".into()))],
            &[
                ("2GLC42sKQveCfGfryNRN9w", "given_name", "John"),
                ("eluV5Og3gSNII8EYnsxA_A", "family_name", "Doe"),
                ("6Ij7tM-a5iVPGboS5tmvVA", "nationality", "BE"),
                ("AJx-095VPrpTtN4QMOqROA", "birthdate", "1990-01-01"),
            ],
        )
    }

    #[test]
    fn full_disclosure_verifies_all_claims() {
        let vk = issuer().verifying_key();
        let sd = sample();
        // Present everything.
        let pres = present(&sd, &["given_name", "family_name", "nationality", "birthdate"]).unwrap();
        let c = verify(&pres, &vk).unwrap();
        assert_eq!(c.get("given_name"), Some("John"));
        assert_eq!(c.get("family_name"), Some("Doe"));
        assert_eq!(c.get("nationality"), Some("BE"));
        assert_eq!(c.get("iss"), Some("https://euro-id.eu"));
    }

    #[test]
    fn selective_disclosure_hides_the_rest() {
        let vk = issuer().verifying_key();
        let sd = sample();
        // Relying party only needs nationality → reveal just that one.
        let pres = present(&sd, &["nationality"]).unwrap();
        let c = verify(&pres, &vk).unwrap();
        assert_eq!(c.get("nationality"), Some("BE"));
        // The undisclosed attributes are NOT recoverable by the verifier.
        assert_eq!(c.get("given_name"), None);
        assert_eq!(c.get("family_name"), None);
        assert_eq!(c.get("birthdate"), None);
    }

    #[test]
    fn wrong_issuer_key_rejected() {
        let sd = sample();
        let pres = present(&sd, &["nationality"]).unwrap();
        let other = SigningKey::from_bytes(&[0x99u8; 32]).verifying_key();
        assert_eq!(verify(&pres, &other), Err(WalletError::BadSignature));
    }

    #[test]
    fn injected_disclosure_rejected() {
        let vk = issuer().verifying_key();
        let sd = sample();
        let mut pres = present(&sd, &["nationality"]).unwrap();
        // Attacker appends a self-made disclosure the issuer never committed.
        let forged = make_disclosure("attackerSaltXXXXXXXXXX", "over_18", "true");
        // Insert before the trailing '~'.
        pres.pop();
        pres.push('~');
        pres.push_str(&forged.encoded);
        pres.push('~');
        assert_eq!(verify(&pres, &vk), Err(WalletError::ForgedDisclosure));
    }

    #[test]
    fn tampered_disclosed_value_rejected() {
        let vk = issuer().verifying_key();
        let sd = sample();
        // Tamper the value inside a committed disclosure → its digest no longer
        // matches the issuer's commitment.
        let mut pres = present(&sd, &["nationality"]).unwrap();
        let good = make_disclosure("6Ij7tM-a5iVPGboS5tmvVA", "nationality", "BE");
        let evil = make_disclosure("6Ij7tM-a5iVPGboS5tmvVA", "nationality", "FR");
        pres = pres.replace(&good.encoded, &evil.encoded);
        assert_eq!(verify(&pres, &vk), Err(WalletError::ForgedDisclosure));
    }

    #[test]
    fn holder_key_binding_roundtrip() {
        let iss = issuer();
        let vk = iss.verifying_key();
        let holder = SigningKey::from_bytes(&[0x55u8; 32]);
        let hpk = holder.verifying_key();
        let sd = sample();
        let pres = present(&sd, &["nationality"]).unwrap();
        let bound = add_key_binding(&pres, &holder, "https://verifier.example", "n-0S6_WzA2Mj");
        // Correct holder + aud + nonce → verifies.
        let c = verify_with_key_binding(&bound, &vk, &hpk, "https://verifier.example", "n-0S6_WzA2Mj").unwrap();
        assert_eq!(c.get("nationality"), Some("BE"));
        // Wrong nonce (replay to a different challenge) → rejected.
        assert_eq!(
            verify_with_key_binding(&bound, &vk, &hpk, "https://verifier.example", "different-nonce"),
            Err(WalletError::BadKeyBinding)
        );
        // A different holder key → rejected.
        let attacker = SigningKey::from_bytes(&[0x77u8; 32]).verifying_key();
        assert_eq!(
            verify_with_key_binding(&bound, &vk, &attacker, "https://verifier.example", "n-0S6_WzA2Mj"),
            Err(WalletError::BadKeyBinding)
        );
    }
}
