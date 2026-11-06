//! # EuroOS app store (sprint N7)
//!
//! `eupkg install <name>` without a USB stick: a SIGNED package index on
//! euro-os.eu, fetched over the same HTTPS path the system updates use
//! (EuroTLS, `net::fetch_bytes`), and packages fetched from the same place.
//!
//! Trust model (the APT model, applied to apps):
//! - the index `/apps/index.json` is signed by the EuroOS key (`crypto::verify`,
//!   the key baked into the kernel); a forged or tampered index is refused
//!   before anything is fetched;
//! - every entry names the developer key (hex) that signed its package; the
//!   signed index VOUCHES for that key, so the package verifies against that
//!   key (or the EuroOS key, or a key the owner enrolled) without the user
//!   enrolling the developer first. A package signed by any other key is
//!   refused even when the index lists it;
//! - the package itself is verified as always (Ed25519 over the manifest,
//!   SHA-256 of the binary), and re-hashed at every launch.
//!
//! The verified index is cached at `/pkg/store-index.json(.sig)` so the
//! launcher can list the store offline; `app store refresh` renews it.

use alloc::string::String;
use alloc::vec::Vec;
use eurofs::FileSystem;
use europkg::PkgIndexEntry;

pub const HOST: &str = "euro-os.eu";
pub const PORT: u16 = 443;
pub const BASE: &str = "/apps";
const CACHE: &str = "/pkg/store-index.json";
const CACHE_SIG: &str = "/pkg/store-index.json.sig";
const MAX_INDEX: usize = 256 * 1024;
const MAX_PKG: usize = 64 << 20;

const TEST_INDEX: &[u8] = include_bytes!("testdata/appindex.json");
const TEST_INDEX_SIG: &[u8] = include_bytes!("testdata/appindex.json.sig");

fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut k = [0u8; 32];
    for i in 0..32 {
        k[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(k)
}

/// Parse an index the EuroOS key signed; None when the signature does not verify.
fn verified_entries(index: &[u8], sig: &[u8]) -> Option<Vec<PkgIndexEntry>> {
    if !crate::crypto::verify(index, sig) {
        return None;
    }
    europkg::store::parse_index(core::str::from_utf8(index).ok()?)
}

/// Fetch the live index and its signature.
pub fn fetch_index() -> Result<(Vec<u8>, Vec<u8>), String> {
    let path = alloc::format!("{BASE}/index.json");
    let index = match crate::net::fetch_bytes(HOST, PORT, &path, true, MAX_INDEX) {
        Some((200, _, b)) => b,
        Some((0, _, _)) => return Err(alloc::format!("store: empty or unparsable HTTP response for {path} (connection dropped?)")),
        Some((code, _, _)) => return Err(alloc::format!("store: HTTP {code} for {path}")),
        None => return Err(String::from("store: no connection to euro-os.eu (network down or not configured)")),
    };
    let sig = match crate::net::fetch_bytes(HOST, PORT, &alloc::format!("{path}.sig"), true, 4096) {
        Some((200, _, b)) => b,
        _ => return Err(String::from("store: index signature not fetched")),
    };
    Ok((index, sig))
}

/// Fetch the index, verify it, cache it. Lines for the shell.
pub fn refresh(fs: &mut dyn FileSystem) -> Vec<String> {
    let (index, sig) = match fetch_index() {
        Ok(v) => v,
        Err(e) => return alloc::vec![e],
    };
    let Some(entries) = verified_entries(&index, &sig) else {
        return alloc::vec![String::from("store: index REFUSED: signature does not verify against the EuroOS key (nothing cached)")];
    };
    // The cache and the package store are SYSTEM data under /pkg; the desktop
    // shell runs with the session user's uid-context (rwx enforced), so write
    // them as the system. The caller is the owner's shell or the launcher.
    let cached = crate::sysctx::as_system(fs, |fs| {
        let _ = fs.create_dir("/pkg");
        fs.write_file(CACHE, &index).is_ok() && fs.write_file(CACHE_SIG, &sig).is_ok()
    });
    if !cached {
        return alloc::vec![String::from("store: index verified but could not be cached under /pkg")];
    }
    alloc::vec![alloc::format!("store: index from https://{HOST}{BASE}/ verified (EuroOS key): {} app(s), cached", entries.len())]
}

/// The cached, verified index (empty when there is none or it does not verify).
pub fn cached_entries(fs: &mut dyn FileSystem) -> Vec<PkgIndexEntry> {
    let (Ok(index), Ok(sig)) = (fs.read_file(CACHE), fs.read_file(CACHE_SIG)) else { return Vec::new() };
    verified_entries(&index, &sig).unwrap_or_default()
}

/// Lines listing the store (cached index) with the installed state.
pub fn list(fs: &mut dyn FileSystem) -> Vec<String> {
    let entries = cached_entries(fs);
    if entries.is_empty() {
        return alloc::vec![String::from("store: no cached index: `app store refresh` (needs the network)")];
    }
    let installed = crate::pkg::installed_apps(fs);
    let mut out = alloc::vec![alloc::format!("EuroOS app store ({} app(s), index signed by the EuroOS key): `app store install <name>`", entries.len())];
    for e in entries {
        let state = match installed.iter().find(|a| a.name == e.name) {
            Some(a) if a.version == e.version => "installed",
            Some(_) => "update available",
            None => "",
        };
        out.push(alloc::format!("  {:<16} {:<8} {:<22} {:<5} {:<10} {}", e.name, e.version, e.title, e.abi, state, e.summary));
    }
    out
}

/// Verify and install `bytes` for index entry `e`: the package must be signed
/// by the key the entry names (or the EuroOS key, or, when `with_trust_store`,
/// a key the owner enrolled). Returns the installed package metadata.
pub fn install_verified(fs: &mut dyn FileSystem, bytes: &[u8], e: &PkgIndexEntry, with_trust_store: bool) -> Result<europkg::store::PkgMeta, String> {
    let Some(key) = hex32(&e.key) else { return Err(alloc::format!("store: entry {} names no valid developer key", e.name)) };
    let mut keys = if with_trust_store { crate::pkg::trusted_keys(fs) } else { Vec::new() };
    keys.push((alloc::format!("store:{}", e.name), key));
    let deps = e.deps.clone();
    crate::pkg::with_engine_keys(fs, keys, move |eng| eng.install_package(bytes, &deps).map_err(|err| alloc::format!("store: {} REFUSED: {err:?}", e.name)))
}

/// `app store install <name>`: index (cached, else fetched), package fetch,
/// verification against the vouched key, install, launcher refresh.
pub fn install(fs: &mut dyn FileSystem, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut entries = cached_entries(fs);
    if entries.is_empty() {
        out.extend(refresh(fs));
        entries = cached_entries(fs);
    }
    let Some(e) = entries.into_iter().find(|e| e.name == name) else {
        out.push(alloc::format!("store: no app '{name}' in the index"));
        return out;
    };
    let file = e.file.rsplit('/').next().unwrap_or(&e.file);
    let path = alloc::format!("{BASE}/{file}");
    let bytes = match crate::net::fetch_bytes(HOST, PORT, &path, true, MAX_PKG) {
        Some((200, _, b)) => b,
        Some((0, _, _)) => { out.push(alloc::format!("store: empty or unparsable HTTP response for {path} (connection dropped?)")); return out; }
        Some((code, _, _)) => { out.push(alloc::format!("store: HTTP {code} for {path}")); return out; }
        None => { out.push(String::from("store: package not fetched (no connection)")); return out; }
    };
    out.push(alloc::format!("store: fetched {path} ({} B)", bytes.len()));
    match crate::sysctx::as_system(fs, |fs| install_verified(fs, &bytes, &e, true)) {
        Ok(m) => {
            crate::launcher::sync_apps(fs);
            out.push(alloc::format!("store: installed {} v{} ({}; caps '{}'; verified: Ed25519 by the key the signed index names + sha256)", m.name, m.version, m.title, m.caps));
        }
        Err(err) => out.push(err),
    }
    out
}

pub fn shell(fs: &mut dyn FileSystem, sub: Option<&str>, arg: Option<&str>) -> Vec<String> {
    match (sub, arg) {
        (None, _) | (Some("list"), _) => list(fs),
        (Some("refresh"), _) => refresh(fs),
        (Some("install"), Some(n)) => install(fs, n),
        _ => alloc::vec![String::from("app store [list] | refresh | install <name>")],
    }
}

/// `[appstore]` boot self-test (offline): the signed test index is accepted,
/// a forged signature is refused before any fetch, an entry whose key is not
/// the key that signed the package is refused even though the index lists it,
/// and a vouched package installs without the developer key being enrolled.
pub fn selftest(fs: &mut dyn FileSystem) {
    let genuine = verified_entries(TEST_INDEX, TEST_INDEX_SIG);
    let parsed = genuine.as_ref().map(|v| v.len() >= 3).unwrap_or(false);
    let mut bad = TEST_INDEX_SIG.to_vec();
    bad[7] ^= 0x55;
    let forged_refused = verified_entries(TEST_INDEX, &bad).is_none();
    let entries = genuine.unwrap_or_default();
    let (wrong_key_refused, vouched_installed) = match (entries.iter().find(|e| e.name == "wrongkey"), entries.iter().find(|e| e.name == "hello-cli")) {
        (Some(wrong), Some(ok)) => {
            let w = install_verified(fs, crate::pkg::hello_euro_eupkg(), wrong, false).is_err();
            let _ = crate::pkg::remove(fs, "hello-cli"); // installed earlier by [app] with an enrolled key
            let v = install_verified(fs, crate::pkg::hello_cli_eupkg(), ok, false).is_ok()
                && crate::pkg::installed_apps(fs).iter().any(|a| a.name == "hello-cli");
            (w, v)
        }
        _ => (false, false),
    };
    let ok = parsed && forged_refused && wrong_key_refused && vouched_installed;
    crate::serial_println!(
        "[appstore] signed app index: parsed={parsed} · forged-signature-refused={forged_refused} · wrong-developer-key-refused={wrong_key_refused} · vouched-package-installed-without-enrolment={vouched_installed} → {}",
        if ok { "OK (index signed by EuroOS vouches for developer keys; packages still verified) ✓" } else { "FAILED" }
    );
}
