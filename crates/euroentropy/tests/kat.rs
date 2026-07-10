//! HMAC-DRBG (SHA-256) verified byte-for-byte against the NIST ACVP known-answer
//! vectors (hmacDRBG, SP 800-90A, no prediction resistance, with reseed). If the
//! DRBG state machine deviates from NIST by a bit, these fail.

fn hx(s: &str) -> Vec<u8> {
    let s = s.trim();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn nist_acvp_hmac_drbg_sha256_kat() {
    let data = include_str!("kat_hmacdrbg.txt");
    let mut cases = 0;
    for block in data.split("---") {
        let mut entropy = Vec::new();
        let mut nonce = Vec::new();
        let mut perso = Vec::new();
        let mut ret_bits = 0usize;
        let mut expected = Vec::new();
        // (op, additional, entropy) in order.
        let mut ops: Vec<(&str, Vec<u8>, Vec<u8>)> = Vec::new();
        for line in block.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(r) = line.strip_prefix("case ret=") {
                ret_bits = r.trim().parse().unwrap();
            } else if let Some(v) = line.strip_prefix("entropy=") {
                entropy = hx(v);
            } else if let Some(v) = line.strip_prefix("nonce=") {
                nonce = hx(v);
            } else if let Some(v) = line.strip_prefix("perso=") {
                perso = hx(v);
            } else if let Some(v) = line.strip_prefix("returnedBits=") {
                expected = hx(v);
            } else if let Some(rest) = line.strip_prefix("reSeed ") {
                let addl = hx(field(rest, "addl="));
                let ent = hx(field(rest, "entropy="));
                ops.push(("reSeed", addl, ent));
            } else if let Some(rest) = line.strip_prefix("generate ") {
                let addl = hx(field(rest, "addl="));
                ops.push(("generate", addl, Vec::new()));
            }
        }
        if entropy.is_empty() {
            continue;
        }
        let mut drbg = euroentropy::HmacDrbg::instantiate(&entropy, &nonce, &perso);
        let mut last = Vec::new();
        for (op, addl, ent) in &ops {
            match *op {
                "reSeed" => drbg.reseed(ent, addl),
                "generate" => last = drbg.generate(ret_bits / 8, addl),
                _ => {}
            }
        }
        assert_eq!(last, expected, "HMAC-DRBG output mismatch");
        cases += 1;
    }
    assert!(cases >= 3, "expected KAT cases missing (got {cases})");
}

fn field<'a>(s: &'a str, key: &str) -> &'a str {
    // Fields are space-separated "k=v"; return the value for `key`.
    for tok in s.split_whitespace() {
        if let Some(v) = tok.strip_prefix(key) {
            return v;
        }
    }
    ""
}
