//! 3E-6 engine tests — install/remove/upgrade/gc against REAL crypto
//! (ed25519-dalek + sha2), with packages and a signed index built in-test.

use std::cell::RefCell;
use std::collections::BTreeMap;

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use europkg::store::{PkgEngine, PkgError, PkgFs};
use europkg::zipread::crc32;
use sha2::{Digest, Sha256};

// ── STORED-zip writer (mirrors what `eupkg build` produces) ──────────────

fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let crc = crc32(data);
        let lho = z.len() as u32;
        z.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        z.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&crc.to_le_bytes());
        z.extend_from_slice(&(data.len() as u32).to_le_bytes());
        z.extend_from_slice(&(data.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&[0, 0]);
        z.extend_from_slice(name.as_bytes());
        z.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend_from_slice(&lho.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_off = z.len() as u32;
    let cd_len = central.len() as u32;
    z.extend_from_slice(&central);
    z.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    z.extend_from_slice(&[0, 0, 0, 0]);
    z.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    z.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    z.extend_from_slice(&cd_len.to_le_bytes());
    z.extend_from_slice(&cd_off.to_le_bytes());
    z.extend_from_slice(&[0, 0]);
    z
}

fn build_pkg(sk: &SigningKey, name: &str, version: &str, bin: &[u8], wrong_hash: bool) -> Vec<u8> {
    let hash = if wrong_hash {
        "00".repeat(32)
    } else {
        hex::encode_local(&Sha256::digest(bin))
    };
    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"{version}\"\nbinary = \"bin/{name}\"\n\n[build]\nbinary_sha256 = \"{hash}\"\n"
    );
    let sig = sk.sign(manifest.as_bytes()).to_bytes();
    stored_zip(&[
        ("MANIFEST.toml", manifest.as_bytes()),
        ("signature.ed25519", &sig),
        (&format!("bin/{name}"), bin),
    ])
}

mod hex {
    pub fn encode_local(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}

// ── in-memory PkgFs ───────────────────────────────────────────────────────

#[derive(Default)]
struct MockFs {
    files: BTreeMap<String, Vec<u8>>,
    links: BTreeMap<String, String>,
}

impl PkgFs for MockFs {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        if let Some(t) = self.links.get(path) {
            return self.files.get(t).cloned();
        }
        self.files.get(path).cloned()
    }
    fn write(&mut self, path: &str, data: &[u8]) -> bool {
        self.files.insert(path.into(), data.to_vec());
        true
    }
    fn exists(&self, path: &str) -> bool {
        self.files.contains_key(path) || self.links.contains_key(path)
    }
    fn remove(&mut self, path: &str) -> bool {
        self.links.remove(path).is_some() | self.files.remove(path).is_some()
    }
    fn mkdir(&mut self, _path: &str) -> bool {
        true
    }
    fn symlink(&mut self, path: &str, target: &str) -> bool {
        self.links.insert(path.into(), target.into());
        true
    }
}

fn keypair() -> (SigningKey, VerifyingKey) {
    let sk = SigningKey::from_bytes(&[7u8; 32]);
    let vk = sk.verifying_key();
    (sk, vk)
}

fn engine_parts() -> (MockFs, SigningKey, VerifyingKey) {
    let (sk, vk) = keypair();
    (MockFs::default(), sk, vk)
}

thread_local! {
    static VK: RefCell<Option<VerifyingKey>> = const { RefCell::new(None) };
}

fn mk_verify(vk: VerifyingKey) -> impl Fn(&[u8], &[u8]) -> bool {
    move |msg, sig| {
        let Ok(s) = ed25519_dalek::Signature::from_slice(sig) else {
            return false;
        };
        vk.verify(msg, &s).is_ok()
    }
}

fn sha(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

#[test]
fn install_links_cas_and_registers() {
    let (mut fs, sk, vk) = engine_parts();
    let verify = mk_verify(vk);
    let pkg = build_pkg(&sk, "hello", "0.1.0", b"BINARY-CONTENT", false);
    let mut eng = PkgEngine { fs: &mut fs, verify: &verify, sha256: &sha, root: "/pkg", bin_dir: "/bin" };
    let meta = eng.install_package(&pkg, &[]).unwrap();
    assert_eq!(meta.name, "hello");
    // /bin/hello resolves (via symlink) to the CAS blob content.
    assert_eq!(eng.fs.read("/bin/hello").as_deref(), Some(b"BINARY-CONTENT".as_ref()));
    let reg = eng.registry();
    assert_eq!(reg.len(), 1);
    // The blob lives in the two-level content-addressed store.
    let h = &reg[0].hash;
    assert!(eng.fs.exists(&format!("/pkg/store/{}/{}", &h[..32], &h[32..])));
}

#[test]
fn tampered_hash_and_forged_signature_refused() {
    let (mut fs, sk, vk) = engine_parts();
    let verify = mk_verify(vk);
    // Manifest pins the wrong hash (but IS validly signed) → HashMismatch.
    let bad_hash = build_pkg(&sk, "evil", "0.1.0", b"X", true);
    let mut eng = PkgEngine { fs: &mut fs, verify: &verify, sha256: &sha, root: "/pkg", bin_dir: "/bin" };
    assert_eq!(eng.install_package(&bad_hash, &[]).unwrap_err(), PkgError::HashMismatch);
    // Signed by a DIFFERENT key → BadSignature.
    let other = SigningKey::from_bytes(&[9u8; 32]);
    let forged = build_pkg(&other, "evil2", "0.1.0", b"X", false);
    assert_eq!(eng.install_package(&forged, &[]).unwrap_err(), PkgError::BadSignature);
    assert!(eng.registry().is_empty());
}

#[test]
fn signed_index_installs_dependency_closure_in_order() {
    let (mut fs, sk, vk) = engine_parts();
    let verify = mk_verify(vk);
    let pkg_a = build_pkg(&sk, "app", "1.0.0", b"APP", false);
    let pkg_b = build_pkg(&sk, "lib", "1.0.0", b"LIB", false);
    let index = br#"{"packages":[
        {"name":"app","version":"1.0.0","file":"/repo/app.eupkg","deps":["lib"]},
        {"name":"lib","version":"1.0.0","file":"/repo/lib.eupkg","deps":[]}]}"#;
    let index_sig = sk.sign(index).to_bytes();

    let mut fetched = Vec::new();
    let mut fetch = |path: &str| -> Option<Vec<u8>> {
        fetched.push(path.to_string());
        match path {
            "/repo/app.eupkg" => Some(pkg_a.clone()),
            "/repo/lib.eupkg" => Some(pkg_b.clone()),
            _ => None,
        }
    };
    let mut eng = PkgEngine { fs: &mut fs, verify: &verify, sha256: &sha, root: "/pkg", bin_dir: "/bin" };
    let done = eng.install_from_index(index, &index_sig, "app", &mut fetch).unwrap();
    // Dependency first (topological), then the app.
    assert_eq!(done.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["lib", "app"]);
    assert!(eng.fs.exists("/bin/lib") && eng.fs.exists("/bin/app"));

    // Removing the dependency while the app needs it is refused.
    assert_eq!(eng.remove("lib").unwrap_err(), PkgError::RequiredBy("app".into()));
    // Remove the app → the lib may go too; gc empties the store.
    eng.remove("app").unwrap();
    eng.remove("lib").unwrap();
    assert!(eng.registry().is_empty());
    assert!(!eng.fs.exists("/bin/app") && !eng.fs.exists("/bin/lib"));
}

#[test]
fn forged_index_fetches_nothing() {
    let (mut fs, sk, vk) = engine_parts();
    let verify = mk_verify(vk);
    let index = br#"{"packages":[{"name":"app","version":"1.0.0","file":"/repo/app.eupkg","deps":[]}]}"#;
    let forged_sig = sk.sign(b"something else").to_bytes();
    let mut calls = 0;
    let mut fetch = |_: &str| -> Option<Vec<u8>> {
        calls += 1;
        None
    };
    let mut eng = PkgEngine { fs: &mut fs, verify: &verify, sha256: &sha, root: "/pkg", bin_dir: "/bin" };
    assert_eq!(eng.install_from_index(index, &forged_sig, "app", &mut fetch).unwrap_err(), PkgError::BadSignature);
    assert_eq!(calls, 0, "a forged index must be refused BEFORE fetching");
}

#[test]
fn upgrade_replaces_and_gcs_old_blob() {
    let (mut fs, sk, vk) = engine_parts();
    let verify = mk_verify(vk);
    let v1 = build_pkg(&sk, "tool", "1.0.0", b"OLD", false);
    let v2 = build_pkg(&sk, "tool", "1.1.0", b"NEW", false);
    let mut eng = PkgEngine { fs: &mut fs, verify: &verify, sha256: &sha, root: "/pkg", bin_dir: "/bin" };
    eng.install_package(&v1, &[]).unwrap();
    let old_hash = eng.registry()[0].hash.clone();

    let index = br#"{"packages":[{"name":"tool","version":"1.1.0","file":"/repo/tool-1.1.eupkg","deps":[]}]}"#;
    let sig = sk.sign(index).to_bytes();
    let mut fetch = |p: &str| -> Option<Vec<u8>> { (p == "/repo/tool-1.1.eupkg").then(|| v2.clone()) };
    let up = eng.upgrade(index, &sig, "tool", &mut fetch).unwrap();
    assert_eq!(up.unwrap().version, "1.1.0");
    assert_eq!(eng.fs.read("/bin/tool").as_deref(), Some(b"NEW".as_ref()));
    // Old blob is unreferenced → gone; same index again → already up to date.
    assert!(!eng.fs.exists(&format!("/pkg/store/{old_hash}")));
    let mut fetch2 = |_: &str| -> Option<Vec<u8>> { None };
    assert!(eng.upgrade(index, &sig, "tool", &mut fetch2).unwrap().is_none());
}
