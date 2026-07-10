//! KAT: euroflate INFLATE must decode REAL `zlib` level-9 raw-deflate streams
//! byte-for-byte (interop — the direction that reads real .docx), and euroflate
//! DEFLATE output must be decodable by REAL `zlib` (via python) so our writer is
//! real-tool-compatible, not merely self-consistent.

use std::process::Command;

fn parse_hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn inflate_decodes_real_zlib_streams() {
    let kat = include_str!("inflate_kat.txt");
    let mut n = 0;
    for line in kat.lines().filter(|l| !l.trim().is_empty()) {
        let parts: Vec<&str> = line.split('|').collect();
        let (name, plain_hex, deflate_hex) = (parts[0], parts[1], parts[2]);
        let plain = parse_hex(plain_hex);
        let comp = parse_hex(deflate_hex);
        let got = euroflate::inflate(&comp).unwrap_or_else(|e| panic!("inflate {name} failed: {e:?}"));
        assert_eq!(got, plain, "INFLATE mismatch for case '{name}'");
        n += 1;
    }
    assert!(n >= 6, "expected the full KAT set");
}

/// euroflate DEFLATE output is read back by REAL zlib (python), proving our
/// writer emits spec-correct streams that LibreOffice/unzip will accept.
#[test]
fn deflate_output_read_by_real_zlib() {
    let fox = b"The quick brown fox jumps over the lazy dog. ".repeat(30);
    let cases: Vec<&[u8]> = vec![
        b"",
        b"a",
        b"Hello, EuroOS! This is a real deflate interop check.",
        b"ABABABABABABABABABABABABABABABABABAB",
        fox.as_slice(),
    ];
    // python may be absent in some CI images; skip gracefully rather than fail.
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("python3 not available — skipping real-zlib interop check");
        return;
    }
    for data in cases {
        let comp = euroflate::deflate(data);
        let hex: String = comp.iter().map(|b| format!("{b:02x}")).collect();
        let expect: String = data.iter().map(|b| format!("{b:02x}")).collect();
        let script = format!(
            "import sys,zlib; raw=bytes.fromhex('{hex}'); out=zlib.decompress(raw,-15); print(out.hex())"
        );
        let output = Command::new("python3").arg("-c").arg(&script).output().expect("run python");
        assert!(output.status.success(), "real zlib rejected our deflate: {}", String::from_utf8_lossy(&output.stderr));
        let got = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(got, expect, "real zlib decoded our deflate to different bytes");
    }
}
