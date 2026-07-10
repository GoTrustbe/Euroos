//! ML-KEM-768 verified byte-for-byte against the NIST ACVP known-answer vectors
//! (ML-KEM-keyGen / ML-KEM-encapDecap, FIPS 203). If any field differs from
//! NIST by a single bit, these fail — this is what makes the implementation
//! interoperable, not merely self-consistent.

fn hx(s: &str) -> Vec<u8> {
    let s = s.trim();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}
fn a32(v: &[u8]) -> [u8; 32] {
    let mut a = [0u8; 32];
    a.copy_from_slice(v);
    a
}
fn field<'a>(parts: &'a [&'a str], key: &str) -> &'a str {
    parts.iter().find_map(|p| p.strip_prefix(key)).expect("missing field")
}

#[test]
fn nist_acvp_ml_kem_768_kat() {
    let data = include_str!("kat_mlkem768.txt");
    let mut keygen = 0;
    let mut encaps = 0;
    let mut decaps = 0;
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts[0] {
            "keygen" => {
                let d = a32(&hx(field(&parts, "d=")));
                let z = a32(&hx(field(&parts, "z=")));
                let ek_exp = hx(field(&parts, "ek="));
                let dk_exp = hx(field(&parts, "dk="));
                let (ek, dk) = europq::keygen(&d, &z);
                assert_eq!(ek, ek_exp, "keygen ek mismatch");
                assert_eq!(dk, dk_exp, "keygen dk mismatch");
                keygen += 1;
            }
            "encaps" => {
                let ek = hx(field(&parts, "ek="));
                let m = a32(&hx(field(&parts, "m=")));
                let c_exp = hx(field(&parts, "c="));
                let k_exp = hx(field(&parts, "k="));
                let (ss, c) = europq::encaps_internal(&ek, &m);
                assert_eq!(c, c_exp, "encaps ciphertext mismatch");
                assert_eq!(ss.to_vec(), k_exp, "encaps shared-secret mismatch");
                encaps += 1;
            }
            "decaps" => {
                let dk = hx(field(&parts, "dk="));
                let c = hx(field(&parts, "c="));
                let k_exp = hx(field(&parts, "k="));
                let ss = europq::decaps(&dk, &c);
                assert_eq!(ss.to_vec(), k_exp, "decaps shared-secret mismatch");
                decaps += 1;
            }
            other => panic!("unknown KAT line: {other}"),
        }
    }
    assert!(keygen >= 3 && encaps >= 3 && decaps >= 3, "expected KAT cases missing");
}
